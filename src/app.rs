//! Application controller: the glue between the tray, the backend, and (in
//! milestone 3) the GTK windows. Runs on the glib main thread; backend work is
//! offloaded to short-lived worker threads that report back via `Cmd::Refresh`.

use crate::backend;
use crate::model::{load_profiles, Profile};
use crate::notify::notify;
use crate::tray::Tray;
use crate::tunnel::{connected_tunnels, uptime_secs};
use gtk::glib;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::Rc;

/// Commands flowing from the tray (and later the GTK UI) into the controller.
#[derive(Debug)]
pub enum Cmd {
    Connect(String),
    Disconnect(String),
    DisconnectAll,
    OpenWindow,
    About,
    Refresh,
    Quit,
    /// No StatusNotifierHost is available, so the tray icon can't be shown.
    /// Sent from the ksni service thread (`Tray::watcher_offline`); the
    /// controller falls back to opening the window so the app stays usable.
    TrayUnavailable,
    /// A StatusNotifierHost (re)appeared after a `TrayUnavailable`; arm the
    /// fallback again so a later loss re-triggers it.
    TrayRestored,
}

/// Main-thread-only mutable state (touched solely from the glib loop).
struct State {
    profiles: Vec<Profile>,
    connected: HashMap<String, (u32, u64)>,
    last_connected: Option<HashSet<String>>, // None until first poll (no launch notification)
    /// Per-tunnel process start time (seconds since boot), captured at each
    /// `refresh()`. Lets `tick()` recompute the elapsed display locally every
    /// second without re-scanning `/proc`.
    start_secs: HashMap<String, f64>,
    /// Live `pidfd` watches keyed by tunnel pid. Holding the `OwnedFd` keeps the
    /// fd alive for its glib source; dropping it (in `reconcile_watches`) closes
    /// it. The glib source is one-shot (returns `Break` on exit), so a dead
    /// watch's source is already gone by the time we drop its fd.
    watches: HashMap<u32, OwnedFd>,
    /// True once the no-tray fallback (window + notification) has fired, so we
    /// don't pop it repeatedly while a host stays absent. Re-armed on `TrayRestored`.
    tray_fallback_shown: bool,
}

/// UI callbacks injected by `main` — no-ops until the GTK UI (milestone 3) sets them.
#[derive(Default)]
pub struct UiHooks {
    pub open_window: Option<Box<dyn Fn()>>,
    pub about: Option<Box<dyn Fn()>>,
    /// Prompt for a one-time 2FA code (returns None if cancelled).
    pub request_otp: Option<Box<dyn Fn(&Profile) -> Option<String>>>,
    /// Tell the UI that state changed so it can redraw any open windows.
    pub on_refresh: Option<Box<dyn Fn()>>,
}

pub struct App {
    state: RefCell<State>,
    /// `None` when the SNI service couldn't start at all (e.g. no D-Bus); the
    /// app then runs window-only and tray snapshot pushes are skipped.
    tray: Option<ksni::blocking::Handle<Tray>>,
    tx: async_channel::Sender<Cmd>,
    hooks: RefCell<UiHooks>,
}

impl App {
    pub fn new(tray: Option<ksni::blocking::Handle<Tray>>, tx: async_channel::Sender<Cmd>) -> Rc<Self> {
        Rc::new(App {
            state: RefCell::new(State {
                profiles: load_profiles(),
                connected: HashMap::new(),
                last_connected: None,
                start_secs: HashMap::new(),
                watches: HashMap::new(),
                tray_fallback_shown: false,
            }),
            tray,
            tx,
            hooks: RefCell::new(UiHooks::default()),
        })
    }

    pub fn set_hooks(&self, hooks: UiHooks) {
        *self.hooks.borrow_mut() = hooks;
    }

    /// A channel sender for other components (UI) to enqueue commands.
    pub fn sender(&self) -> async_channel::Sender<Cmd> {
        self.tx.clone()
    }

    /// Heavy path: scan `/proc` for live tunnels, fire connect/disconnect
    /// notifications on change, cache start times, (re)arm `pidfd` drop-watches,
    /// push the tray snapshot, and ask the UI to redraw. Runs at startup, after
    /// connect/disconnect, on a `pidfd` exit, and on a slow safety-net timer —
    /// NOT every second (that's `tick()`, which does no scan).
    pub fn refresh(&self) {
        let profiles = load_profiles();
        let connected = connected_tunnels(&profiles);
        let names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
        let now: HashSet<String> = connected.keys().cloned().collect();

        // Cache each tunnel's start instant (seconds since boot) so `tick()` can
        // recompute elapsed locally: start = current uptime − measured elapsed.
        let uptime = uptime_secs();
        let start_secs: HashMap<String, f64> =
            connected.iter().map(|(n, (_, e))| (n.clone(), uptime - *e as f64)).collect();

        // Notify per profile on change (manual connects + unexpected drops).
        {
            let mut st = self.state.borrow_mut();
            if let Some(prev) = &st.last_connected {
                for name in now.difference(prev) {
                    notify("VPN connected", &format!("Connected to {name}."));
                }
                let closed: Vec<&String> = prev.difference(&now).collect();
                for name in &closed {
                    notify("VPN disconnected", &format!("Disconnected from {name}."));
                }
                // When any tunnel closes, sweep stale per-tunnel state a crashed
                // daemon may have left behind (only when the helper is installed,
                // so dev runs don't trigger a polkit prompt).
                if !closed.is_empty() {
                    if let Some(helper) = crate::sys::disconnect_helper() {
                        std::thread::spawn(move || {
                            let _ = crate::privilege::run_root(helper, &["sweep"], None);
                        });
                    }
                }
            }
            st.profiles = profiles;
            st.connected = connected.clone();
            st.start_secs = start_secs;
            st.last_connected = Some(now);
        }

        // (Re)arm a pidfd exit-watch per live tunnel; drop watches for gone pids.
        self.reconcile_watches(&connected);

        // Push a fresh snapshot to the tray (skipped if the tray never started).
        if let Some(tray) = &self.tray {
            tray.update(move |t: &mut Tray| {
                t.profiles = names.clone();
                t.connected = connected.clone();
            });
        }

        if let Some(cb) = &self.hooks.borrow().on_refresh {
            cb();
        }
    }

    /// Cheap per-second display update: recompute each live tunnel's elapsed
    /// time locally from its cached start (one tiny `/proc/uptime` read, no
    /// process scan) and push the snapshot to the tray. The window ticks its own
    /// labels separately, so we don't fire the `on_refresh` UI rebuild here.
    pub fn tick(&self) {
        let snapshot = {
            let mut st = self.state.borrow_mut();
            if st.connected.is_empty() {
                return; // nothing live → no I/O, no push
            }
            let uptime = uptime_secs();
            let starts = st.start_secs.clone();
            for (name, entry) in st.connected.iter_mut() {
                if let Some(start) = starts.get(name) {
                    entry.1 = (uptime - start).max(0.0) as u64;
                }
            }
            st.connected.clone()
        };
        if let Some(tray) = &self.tray {
            tray.update(move |t: &mut Tray| t.connected = snapshot.clone());
        }
    }

    /// Open a one-shot `pidfd` glib watch for each newly-live tunnel and close
    /// watches whose pid is gone. When a watched process exits, glib wakes the
    /// closure, which enqueues `Cmd::Refresh` (→ `refresh()` notices the drop,
    /// notifies, and reconciles). Must run on the glib main thread.
    fn reconcile_watches(&self, connected: &HashMap<String, (u32, u64)>) {
        let current: HashSet<u32> = connected.values().map(|(pid, _)| *pid).collect();
        let mut st = self.state.borrow_mut();
        // Drop fds for pids no longer live. Normal case: the process exited, so
        // its one-shot source already removed itself (Break) — closing the fd is
        // clean. Rare case (a live process that stopped matching a profile, e.g.
        // after a rename): a stale source may remain on the closed fd, but it can
        // only fire a spurious Cmd::Refresh, which re-scans truth — self-correcting.
        st.watches.retain(|pid, _| current.contains(pid));
        for &pid in &current {
            if st.watches.contains_key(&pid) {
                continue;
            }
            if let Some(fd) = crate::pidfd::open(pid) {
                let tx = self.tx.clone();
                glib::unix_fd_add_local(fd.as_raw_fd(), glib::IOCondition::IN, move |_, _| {
                    let _ = tx.send_blocking(Cmd::Refresh);
                    glib::ControlFlow::Break // one-shot: process is gone
                });
                st.watches.insert(pid, fd);
            }
        }
    }

    /// Read-only snapshot of profiles (for the UI).
    pub fn profiles(&self) -> Vec<Profile> {
        self.state.borrow().profiles.clone()
    }

    pub fn connected(&self) -> HashMap<String, (u32, u64)> {
        self.state.borrow().connected.clone()
    }

    fn find(&self, name: &str) -> Option<Profile> {
        self.state.borrow().profiles.iter().find(|p| p.name == name).cloned()
    }

    /// Run a backend op on a worker thread, then trigger a refresh.
    fn spawn<F>(&self, op: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            op();
            let _ = tx.send_blocking(Cmd::Refresh);
        });
    }

    /// Handle one command (called from the glib loop).
    pub fn handle(self: &Rc<Self>, cmd: Cmd) {
        match cmd {
            Cmd::Connect(name) => {
                let Some(p) = self.find(&name) else { return };
                // openconnect profiles needing 2FA prompt on the main thread first.
                let otp = if p.is_openconnect() && p.oc_otp.unwrap_or(false) {
                    match self.hooks.borrow().request_otp.as_ref() {
                        Some(prompt) => match prompt(&p) {
                            Some(code) => Some(code),
                            None => return, // cancelled
                        },
                        None => None,
                    }
                } else {
                    None
                };
                self.spawn(move || {
                    let _ = backend::connect(&p, otp.as_deref());
                });
            }
            Cmd::Disconnect(name) => {
                let Some(p) = self.find(&name) else { return };
                self.spawn(move || {
                    let _ = backend::disconnect(&p);
                });
            }
            Cmd::DisconnectAll => {
                let connected = self.connected();
                let to_drop: Vec<Profile> =
                    self.profiles().into_iter().filter(|p| connected.contains_key(&p.name)).collect();
                self.spawn(move || {
                    for p in &to_drop {
                        let _ = backend::disconnect(p);
                    }
                });
            }
            Cmd::OpenWindow => {
                if let Some(cb) = &self.hooks.borrow().open_window {
                    cb();
                }
            }
            Cmd::About => {
                if let Some(cb) = &self.hooks.borrow().about {
                    cb();
                }
            }
            Cmd::Refresh => self.refresh(),
            Cmd::TrayUnavailable => {
                // Fire once per offline episode: surface the window so the app is
                // still operable without a tray icon, and tell the user why.
                let already = {
                    let mut st = self.state.borrow_mut();
                    let was = st.tray_fallback_shown;
                    st.tray_fallback_shown = true;
                    was
                };
                if !already {
                    if let Some(cb) = &self.hooks.borrow().open_window {
                        cb();
                    }
                    notify(
                        "VpncBar: no system tray",
                        "No status-tray host was found, so the tray icon can't be shown. \
                         The VpncBar window is open instead. On GNOME, enable the \
                         AppIndicator/KStatusNotifierItem extension to get the tray icon.",
                    );
                }
            }
            Cmd::TrayRestored => {
                // A host came back; re-arm so a future loss pops the fallback again.
                self.state.borrow_mut().tray_fallback_shown = false;
            }
            Cmd::Quit => {
                self.disconnect_all_sync();
                std::process::exit(0);
            }
        }
    }

    /// Disconnect every live tunnel synchronously — for quit / SIGTERM, so we
    /// don't orphan tunnels (mirrors the macOS teardown).
    pub fn disconnect_all_sync(&self) {
        let profiles = load_profiles();
        let connected = connected_tunnels(&profiles);
        for p in profiles.iter().filter(|p| connected.contains_key(&p.name)) {
            let _ = backend::disconnect(p);
        }
    }
}
