//! Application controller: the glue between the tray, the backend, and (in
//! milestone 3) the GTK windows. Runs on the glib main thread; backend work is
//! offloaded to short-lived worker threads that report back via `Cmd::Refresh`.

use crate::backend;
use crate::model::{load_profiles, Profile};
use crate::notify::notify;
use crate::tray::Tray;
use crate::tunnel::connected_tunnels;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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
}

/// Main-thread-only mutable state (touched solely from the glib loop).
struct State {
    profiles: Vec<Profile>,
    connected: HashMap<String, (u32, u64)>,
    last_connected: Option<HashSet<String>>, // None until first poll (no launch notification)
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
    tray: ksni::Handle<Tray>,
    tx: async_channel::Sender<Cmd>,
    hooks: RefCell<UiHooks>,
}

impl App {
    pub fn new(tray: ksni::Handle<Tray>, tx: async_channel::Sender<Cmd>) -> Rc<Self> {
        Rc::new(App {
            state: RefCell::new(State {
                profiles: load_profiles(),
                connected: HashMap::new(),
                last_connected: None,
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

    /// Re-read profiles + live tunnels, push to the tray, fire connect/disconnect
    /// notifications on change, and ask the UI to redraw.
    pub fn refresh(&self) {
        let profiles = load_profiles();
        let connected = connected_tunnels(&profiles);
        let names: Vec<String> = profiles.iter().map(|p| p.name.clone()).collect();
        let now: HashSet<String> = connected.keys().cloned().collect();

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
            st.last_connected = Some(now);
        }

        // Push a fresh snapshot to the tray.
        self.tray.update(move |t: &mut Tray| {
            t.profiles = names.clone();
            t.connected = connected.clone();
        });

        if let Some(cb) = &self.hooks.borrow().on_refresh {
            cb();
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
