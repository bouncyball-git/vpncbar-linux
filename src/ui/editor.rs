// The enumerated fields use gtk::DropDown (via the IdDropDown wrapper below).
// They were ComboBoxText, but its popover grab swallowed the first click after
// use — Save needed two clicks. Only oc_authgroup remains a ComboBoxText: it's
// the editable (with-entry) variant, which DropDown doesn't offer.
#![allow(deprecated)]

//! Profile editor window — the GTK port of the macOS profile editor sheet.
//! A Type selector switches between vpnc (Cisco IPSec) and openconnect
//! (AnyConnect SSL) fields. Secrets are loaded from / saved to the Secret
//! Service; never written to profiles.json.

use crate::app::{App, Cmd};
use crate::model::{ne, upsert, Profile};
use crate::secrets;
use crate::tunnel::is_connected;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

thread_local! {
    /// One editor window per profile (keyed by uuid; new profiles share the
    /// "__new__" slot), so re-opening an editor just brings it forward instead
    /// of spawning a duplicate. Mirrors the macOS per-uuid editor registry.
    static OPEN_EDITORS: RefCell<std::collections::HashMap<String, Rc<Editor>>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Open the editor for an existing profile, or a new one (`None`). If an editor
/// for the same profile is already open, it's presented rather than duplicated.
pub fn open_editor(app: &Rc<App>, parent: &gtk::Window, profile: Option<Profile>) {
    let key = profile
        .as_ref()
        .and_then(|p| p.uuid.clone())
        .unwrap_or_else(|| "__new__".to_string());

    if let Some(ed) = OPEN_EDITORS.with(|m| m.borrow().get(&key).cloned()) {
        ed.window.present();
        return;
    }

    let ed = Editor::new(app, parent, profile);
    OPEN_EDITORS.with(|m| m.borrow_mut().insert(key.clone(), ed.clone()));
    // Drop the registry entry when the window closes so it can be reopened.
    ed.window.connect_close_request(move |_| {
        OPEN_EDITORS.with(|m| {
            m.borrow_mut().remove(&key);
        });
        glib::Propagation::Proceed
    });
    ed.present();
}

/// Push the live connection state into every open editor (Connect/Disconnect
/// button). Called from the controller's refresh — i.e. exactly when the state
/// changes (connect/disconnect commands, a pidfd drop, the safety-net scan) —
/// instead of each editor polling for it.
pub fn refresh_open_editors() {
    OPEN_EDITORS.with(|m| {
        for ed in m.borrow().values() {
            ed.refresh_connect_button();
        }
    });
}

/// Holds every input widget so Save can read them back.
struct Fields {
    // shared
    name: gtk::Entry,
    gateway: gtk::Entry,
    username: gtk::Entry,
    password: gtk::PasswordEntry,
    domains: gtk::Entry,
    client_cert: gtk::Entry,
    // vpnc
    id: gtk::Entry,
    secret: gtk::PasswordEntry,
    authmode: IdDropDown,
    /// Shown when cert/hybrid is chosen on a vpnc built without GnuTLS.
    auth_note: gtk::Label,
    ca_file: gtk::Entry,
    dh_group: IdDropDown,
    pfs: IdDropDown,
    nat_mode: IdDropDown,
    vendor: IdDropDown,
    mtu: gtk::Entry,
    dpd_timeout: gtk::Entry,
    debug: IdDropDown,
    enable_weak: gtk::CheckButton,
    single_des: gtk::CheckButton,
    no_encryption: gtk::CheckButton,
    weak_auth: gtk::CheckButton,
    // openconnect
    oc_authgroup: gtk::ComboBoxText,
    oc_server_cert: gtk::Entry,
    oc_otp: gtk::CheckButton,
    oc_protocol: IdDropDown,
    oc_no_dtls: gtk::CheckButton,
    oc_dpd: gtk::Entry,
    oc_mtu: gtk::Entry,
    oc_reconnect: gtk::Entry,
    oc_debug: IdDropDown,
    oc_fetch: gtk::Button,
    // Info / Debug tabs (display-only).
    info_label: gtk::Label,
    debug_view: gtk::TextView,
    debug_clear: gtk::Button,
    debug_reveal: gtk::Button,
}

struct Editor {
    window: gtk::Window,
    app: Rc<App>,
    profile: RefCell<Profile>, // carries uuid across save
    fields: Fields,
    connect_btn: gtk::Button,
    /// openconnect group -> needs-2FA, from the last Fetch groups probe.
    groups: RefCell<std::collections::HashMap<String, bool>>,
    /// In-flight connect/disconnect: (target_connected, started). Drives the
    /// Info tab's transient "Connecting…/Disconnecting…" status (20s deadline),
    /// mirroring the macOS editor so the status doesn't flicker through the
    /// stale state. Cleared once the real state reaches the target or times out.
    transition: RefCell<Option<(bool, std::time::Instant)>>,
    /// True while a vpnc journal fetch is in flight, so the 1s Debug refresh
    /// doesn't pile up `journalctl` calls. `Rc<Cell>` so the async result handler
    /// can clear it without borrowing the whole editor.
    journal_busy: Rc<Cell<bool>>,
    /// "Clear log" cutoff (unix seconds) for vpnc: the journal can't be
    /// truncated, so clearing means showing only lines newer than this.
    clear_cutoff: Cell<Option<u64>>,
    /// Last live vpnc pid, kept across the drop so the first refresh after a
    /// disconnect can run one final journal sync (capturing the teardown lines)
    /// before the tunnel's journal scope is forgotten.
    last_vpnc_pid: Cell<Option<u32>>,
}

fn entry(text: Option<&str>, placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_hexpand(true);
    if let Some(t) = text {
        e.set_text(t);
    }
    if !placeholder.is_empty() {
        e.set_placeholder_text(Some(placeholder));
    }
    e
}

/// Id-keyed wrapper over `gtk::DropDown`, mimicking ComboBoxText's id API.
/// Derefs to the DropDown so widget ops (`set_sensitive`, `upcast_ref`, …)
/// work unchanged at the call sites.
#[derive(Clone)]
struct IdDropDown {
    dd: gtk::DropDown,
    ids: Rc<Vec<String>>,
}

impl std::ops::Deref for IdDropDown {
    type Target = gtk::DropDown;
    fn deref(&self) -> &gtk::DropDown {
        &self.dd
    }
}

impl IdDropDown {
    fn new(items: &[(&str, &str)], active: Option<&str>) -> Self {
        let labels: Vec<&str> = items.iter().map(|(_, l)| *l).collect();
        let dd = gtk::DropDown::from_strings(&labels);

        // Plain-label factory for both the button and the popup rows: the
        // default one puts a checkmark on the selected row, which widens the
        // popup past the button. Without it the two render the same width.
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, obj| {
            if let Some(item) = obj.downcast_ref::<gtk::ListItem>() {
                let label = gtk::Label::new(None);
                label.set_xalign(0.0);
                item.set_child(Some(&label));
            }
        });
        factory.connect_bind(|_, obj| {
            if let Some(item) = obj.downcast_ref::<gtk::ListItem>() {
                if let (Some(label), Some(s)) = (
                    item.child().and_downcast::<gtk::Label>(),
                    item.item().and_downcast::<gtk::StringObject>(),
                ) {
                    label.set_text(&s.string());
                }
            }
        });
        dd.set_factory(Some(&factory));

        // Size the closed button to the WIDEST item: DropDown only sizes it to
        // the selected one, so it came up narrower than its popup (and jumped on
        // selection change). Measured once on first map (when the theme CSS is
        // applied): chrome = natural − selected-label width, then request
        // widest-label + chrome.
        {
            let labels: Vec<String> = labels.iter().map(|s| s.to_string()).collect();
            let done = std::cell::Cell::new(false);
            dd.connect_map(move |dd| {
                if done.replace(true) {
                    return;
                }
                let (_, nat, _, _) = dd.measure(gtk::Orientation::Horizontal, -1);
                let text_w = |s: &str| dd.create_pango_layout(Some(s)).pixel_size().0;
                let sel_w = labels.get(dd.selected() as usize).map(|s| text_w(s)).unwrap_or(0);
                let max_w = labels.iter().map(|s| text_w(s)).max().unwrap_or(sel_w);
                dd.set_size_request(nat - sel_w + max_w, -1);
            });
        }

        let ids = Rc::new(items.iter().map(|(id, _)| id.to_string()).collect::<Vec<_>>());
        let me = IdDropDown { dd, ids };
        me.set_active_id(active);
        me
    }

    fn active_id(&self) -> Option<String> {
        self.ids.get(self.dd.selected() as usize).cloned()
    }

    fn set_active_id(&self, id: Option<&str>) {
        // Unlike ComboBox, a DropDown always has a selection; unknown → first.
        let pos = id.and_then(|id| self.ids.iter().position(|x| x == id)).unwrap_or(0);
        self.dd.set_selected(pos as u32);
    }

    fn connect_changed<F: Fn(&Self) + 'static>(&self, f: F) {
        let me = self.clone();
        self.dd.connect_selected_notify(move |_| f(&me));
    }
}

fn combo(items: &[(&str, &str)], active: Option<&str>) -> IdDropDown {
    IdDropDown::new(items, active.or(items.first().map(|(id, _)| *id)))
}

/// A dropdown whose first entry is an empty "(default)" choice, so leaving it
/// alone omits the directive (matching the free-text "blank = backend default"
/// behaviour) while still offering the valid values. `active` selects an
/// existing value, else "(default)".
fn combo_default(items: &[&str], active: Option<&str>) -> IdDropDown {
    let mut all: Vec<(&str, &str)> = vec![("", "(default)")];
    all.extend(items.iter().map(|i| (*i, *i)));
    let c = IdDropDown::new(&all, active.filter(|s| !s.is_empty()).or(Some("")));
    c.set_hexpand(true);
    c
}

/// Add a "label: widget" row to a grid at `row`.
fn add_row(grid: &gtk::Grid, row: i32, label: &str, w: &impl IsA<gtk::Widget>) {
    let l = gtk::Label::new(Some(label));
    l.set_xalign(1.0);
    l.add_css_class("dim-label");
    grid.attach(&l, 0, row, 1, 1);
    grid.attach(w, 1, row, 1, 1);
}

/// Replace a TextView's contents only if they changed (avoids resetting the
/// scroll/cursor every refresh).
fn set_view_text(view: &gtk::TextView, text: &str) {
    let buf = view.buffer();
    if buf.text(&buf.start_iter(), &buf.end_iter(), false) != text {
        buf.set_text(text);
    }
}

fn form_grid() -> gtk::Grid {
    let g = gtk::Grid::new();
    g.set_row_spacing(8);
    g.set_column_spacing(10);
    g.set_margin_top(12);
    g.set_margin_bottom(12);
    g.set_margin_start(12);
    g.set_margin_end(12);
    g
}

impl Editor {
    fn new(app: &Rc<App>, parent: &gtk::Window, profile: Option<Profile>) -> Rc<Editor> {
        let is_new = profile.is_none();
        let p = profile.unwrap_or_default();
        let kind = if p.is_openconnect() { "openconnect" } else { "vpnc" };

        // Pre-load existing secrets so the fields show what's stored.
        let stored_secret = if p.uuid.is_some() {
            secrets::get(&secrets::kc_service(&p, "secret")).unwrap_or_default()
        } else {
            String::new()
        };
        let stored_password = if p.uuid.is_some() {
            secrets::get(&secrets::kc_service(&p, "password")).unwrap_or_default()
        } else {
            String::new()
        };

        let window = gtk::Window::builder()
            .title(if is_new { "New VPN".to_string() } else { format!("Edit “{}”", p.name) })
            .transient_for(parent)
            .modal(false)
            .default_width(460)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // Type selector (locked once a profile exists).
        let type_bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        type_bar.set_margin_top(10);
        type_bar.set_margin_bottom(10); // breathing room before the notebook tabs
        type_bar.set_margin_start(12);
        type_bar.set_margin_end(12);
        let type_label = gtk::Label::new(Some("Type:"));
        let type_combo = combo(
            &[("vpnc", "Cisco IPSec (vpnc)"), ("openconnect", "AnyConnect (openconnect)")],
            Some(kind),
        );
        type_combo.set_sensitive(is_new);
        type_bar.append(&type_label);
        type_bar.append(&*type_combo);
        root.append(&type_bar);

        // Build all widgets.
        let pw = |val: &str| {
            let e = gtk::PasswordEntry::new();
            e.set_show_peek_icon(true);
            e.set_hexpand(true);
            if !val.is_empty() {
                e.set_text(val);
            }
            e
        };
        let fields = Fields {
            name: entry(Some(&p.name), "Display name"),
            gateway: entry(Some(&p.gateway), "vpn.example.com"),
            username: entry(Some(&p.username), "user or DOMAIN\\user"),
            password: pw(&stored_password),
            domains: entry(p.dns_match_domains.as_deref(), "corp.local, example.com"),
            client_cert: entry(p.client_cert.as_deref(), "path to client cert (optional)"),
            id: entry(Some(&p.id), "group name"),
            secret: pw(&stored_secret),
            authmode: combo(
                &[("psk", "psk"), ("hybrid", "hybrid"), ("cert", "cert")],
                p.authmode.as_deref().or(Some("psk")),
            ),
            auth_note: {
                let l = gtk::Label::new(None);
                l.set_xalign(0.0);
                l.set_wrap(true);
                l.add_css_class("dim-label");
                l.set_visible(false);
                l
            },
            ca_file: entry(p.ca_file.as_deref(), "CA file (cert/hybrid)"),
            dh_group: combo_default(
                &["dh1", "dh2", "dh5", "dh14", "dh15", "dh16", "dh17", "dh18"],
                p.dh_group.as_deref(),
            ),
            pfs: combo_default(
                &["nopfs", "dh1", "dh2", "dh5", "dh14", "dh15", "dh16", "dh17", "dh18", "server"],
                p.pfs.as_deref(),
            ),
            nat_mode: combo_default(&["natt", "none", "force-natt", "cisco-udp"], p.nat_mode.as_deref()),
            vendor: combo_default(&["cisco", "netscreen", "fortigate"], p.vendor.as_deref()),
            mtu: entry(p.mtu.as_deref(), "auto"),
            dpd_timeout: entry(p.dpd_timeout.as_deref(), "30"),
            debug: combo(
                &[("0", "0"), ("1", "1"), ("2", "2"), ("3", "3"), ("99", "99")],
                p.debug.as_deref().or(Some("0")),
            ),
            enable_weak: gtk::CheckButton::with_label("Enable weak encryption (3DES)"),
            single_des: gtk::CheckButton::with_label("Enable Single DES"),
            no_encryption: gtk::CheckButton::with_label("Enable no encryption"),
            weak_auth: gtk::CheckButton::with_label("Enable weak authentication"),
            oc_authgroup: {
                let c = gtk::ComboBoxText::with_entry();
                if let Some(g) = ne(p.oc_authgroup.as_deref()) {
                    c.append(Some(&g), &g);
                    c.set_active_id(Some(&g));
                }
                c.set_hexpand(true);
                c
            },
            oc_server_cert: entry(p.oc_server_cert.as_deref(), "pin-sha256:… (optional)"),
            oc_otp: gtk::CheckButton::with_label("Ask for one-time code (2FA)"),
            oc_protocol: combo(
                &[
                    ("anyconnect", "anyconnect"), ("gp", "gp"), ("pulse", "pulse"),
                    ("f5", "f5"), ("fortinet", "fortinet"), ("nc", "nc"), ("array", "array"),
                ],
                p.oc_protocol.as_deref().or(Some("anyconnect")),
            ),
            oc_no_dtls: gtk::CheckButton::with_label("Force TLS (disable DTLS/UDP)"),
            oc_dpd: entry(p.oc_dpd.as_deref(), "gateway default"),
            oc_mtu: entry(p.oc_mtu.as_deref(), "auto"),
            oc_reconnect: entry(p.oc_reconnect.as_deref(), "300"),
            oc_debug: combo(
                &[("0", "0"), ("1", "1"), ("2", "2"), ("3", "3"), ("99", "99")],
                p.oc_debug.as_deref().or(Some("1")),
            ),
            oc_fetch: gtk::Button::with_label("Fetch groups"),
            info_label: {
                let l = gtk::Label::new(Some("Not connected."));
                l.set_xalign(0.0);
                l.set_yalign(0.0);
                l.set_selectable(true);
                // Don't take keyboard focus: a selectable label auto-selects ALL
                // its text on focus-in, which painted the whole Info block blue
                // when the tab opened. Mouse drag-selection still works for copy.
                l.set_can_focus(false);
                l.set_wrap(true);
                l
            },
            debug_view: {
                let v = gtk::TextView::new();
                v.set_editable(false);
                v.set_monospace(true);
                v
            },
            debug_clear: gtk::Button::with_label("Clear log"),
            debug_reveal: gtk::Button::with_label("Reveal log"),
        };
        fields.enable_weak.set_active(p.enable_weak.unwrap_or(true));
        fields.single_des.set_active(p.single_des.unwrap_or(false));
        fields.no_encryption.set_active(p.no_encryption.unwrap_or(false));
        fields.weak_auth.set_active(p.weak_auth.unwrap_or(false));
        fields.oc_otp.set_active(p.oc_otp.unwrap_or(false));
        fields.oc_no_dtls.set_active(p.oc_no_dtls.unwrap_or(false));

        // Notebook with per-type pages.
        let notebook = gtk::Notebook::new();
        notebook.set_margin_start(6);
        notebook.set_margin_end(6);
        build_pages(&notebook, kind, &fields);
        root.append(&notebook);

        // Footer: Connect/Disconnect + Save + Cancel.
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        footer.set_margin_top(8);
        footer.set_margin_bottom(12);
        footer.set_margin_start(12);
        footer.set_margin_end(12);
        let connect_btn = gtk::Button::new();
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let cancel = gtk::Button::with_label("Cancel");
        let save = gtk::Button::with_label("Save");
        save.add_css_class("suggested-action");
        footer.append(&connect_btn);
        footer.append(&spacer);
        footer.append(&cancel);
        footer.append(&save);
        root.append(&footer);

        window.set_child(Some(&root));

        let ed = Rc::new(Editor {
            window: window.clone(),
            app: app.clone(),
            profile: RefCell::new(p),
            fields,
            connect_btn: connect_btn.clone(),
            groups: RefCell::new(std::collections::HashMap::new()),
            transition: RefCell::new(None),
            journal_busy: Rc::new(Cell::new(false)),
            clear_cutoff: Cell::new(None),
            last_vpnc_pid: Cell::new(None),
        });

        // Rebuild pages when the type changes (new profiles only).
        {
            let ed = ed.clone();
            let notebook = notebook.clone();
            type_combo.connect_changed(move |c| {
                let kind = c.active_id().map(|s| s.to_string()).unwrap_or_else(|| "vpnc".into());
                ed.profile.borrow_mut().kind = Some(kind.clone());
                build_pages(&notebook, &kind, &ed.fields);
                ed.apply_authmode();
            });
        }
        // Gray out credential fields that don't apply to the chosen IKE Authmode.
        {
            let ed = ed.clone();
            ed.clone().fields.authmode.connect_changed(move |_| ed.apply_authmode());
        }
        {
            let ed = ed.clone();
            cancel.connect_clicked(move |_| ed.window.close());
        }
        {
            let ed = ed.clone();
            save.connect_clicked(move |_| {
                if ed.save() {
                    ed.window.close();
                }
            });
        }
        {
            let ed = ed.clone();
            connect_btn.connect_clicked(move |_| ed.toggle_connection());
        }
        // openconnect: Fetch groups probes the gateway and fills the dropdown.
        {
            let ed = ed.clone();
            ed.clone().fields.oc_fetch.connect_clicked(move |_| ed.fetch_groups());
        }
        // Selecting a group auto-ticks "Ask for one-time code" when it needs 2FA.
        {
            let ed = ed.clone();
            ed.clone().fields.oc_authgroup.connect_changed(move |c| {
                if let Some(g) = c.active_text() {
                    if let Some(needs) = ed.groups.borrow().get(g.as_str()) {
                        ed.fields.oc_otp.set_active(*needs);
                    }
                }
            });
        }
        {
            let ed = ed.clone();
            ed.clone().fields.debug_clear.connect_clicked(move |_| ed.clear_log());
        }
        {
            let ed = ed.clone();
            ed.clone().fields.debug_reveal.connect_clicked(move |_| ed.reveal_log());
        }
        // Refresh the Info/Debug tabs once a second while the editor is shown.
        {
            let ed = ed.clone();
            glib::timeout_add_seconds_local(1, move || {
                if ed.window.is_visible() {
                    ed.update_info_debug();
                }
                glib::ControlFlow::Continue
            });
        }

        // Return = Save (default widget), Esc = Cancel/close.
        window.set_default_widget(Some(&save));
        {
            let key = gtk::EventControllerKey::new();
            let ed = ed.clone();
            key.connect_key_pressed(move |_, keyval, _, _| {
                if keyval == gtk::gdk::Key::Escape {
                    ed.window.close();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            });
            window.add_controller(key);
        }

        ed.apply_authmode();
        ed.refresh_connect_button();
        ed.update_info_debug();
        ed
    }

    /// Enable only the credential fields that apply to the current backend /
    /// IKE Authmode, fading the rest — and warn if cert/hybrid is picked on a
    /// vpnc without GnuTLS. Mirrors the macOS `authModeChanged`.
    fn apply_authmode(&self) {
        let f = &self.fields;
        if self.is_oc() {
            f.secret.set_sensitive(true);
            f.ca_file.set_sensitive(true);
            f.client_cert.set_sensitive(true);
            f.auth_note.set_visible(false);
            return;
        }
        let mode = f.authmode.active_id().map(|s| s.to_string()).unwrap_or_else(|| "psk".into());
        let (is_cert, is_hybrid) = (mode == "cert", mode == "hybrid");
        f.secret.set_sensitive(mode == "psk");
        f.ca_file.set_sensitive(is_cert || is_hybrid);
        f.client_cert.set_sensitive(is_cert);
        if (is_cert || is_hybrid) && !crate::sys::vpnc_supports_certs() {
            f.auth_note.set_text(&format!(
                "This vpnc build has no certificate support (not linked against GnuTLS), \
                 so “{mode}” mode won’t take effect. Rebuild vpnc with GnuTLS to use it."
            ));
            f.auth_note.set_visible(true);
        } else {
            f.auth_note.set_visible(false);
        }
    }

    /// Probe the gateway for its group list (off the main thread) and populate
    /// the Auth group dropdown + remember each group's 2FA flag.
    fn fetch_groups(self: &Rc<Self>) {
        let server = self.fields.gateway.text().trim().to_string();
        if server.is_empty() {
            return;
        }
        let cert = ne(Some(&self.fields.oc_server_cert.text()));
        self.fields.oc_fetch.set_label("Fetching…");
        self.fields.oc_fetch.set_sensitive(false);

        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let probe = crate::backend::openconnect::group_list(&server, cert.as_deref());
            let _ = tx.send_blocking(probe);
        });

        let ed = self.clone();
        glib::spawn_future_local(async move {
            let Ok(probe) = rx.recv().await else { return };
            ed.fields.oc_fetch.set_label("Fetch groups");
            ed.fields.oc_fetch.set_sensitive(true);

            // Trust-on-first-use: the gateway presented an untrusted cert and no
            // pin is set. Show the fingerprint; on consent, pin it and retry — the
            // pin is enforced on every connection thereafter (warned if it changes).
            if let Some(pin) = probe.cert_pin {
                let ed2 = ed.clone();
                let pin2 = pin.clone();
                gtk::AlertDialog::builder()
                    .modal(true)
                    .message("Untrusted server certificate")
                    .detail(format!(
                        "The gateway “{}” presented a certificate that isn't trusted \
                         (self-signed, or from a private CA).\n\nFingerprint:\n{pin}\n\n\
                         Trust and pin this certificate? It's saved to the profile and \
                         required on every future connection — you'll be warned if it \
                         ever changes.",
                        ed.fields.gateway.text().trim()
                    ))
                    .buttons(["Cancel", "Trust & Pin"])
                    .cancel_button(0)
                    .default_button(1)
                    .build()
                    .choose(Some(&ed.window), gtk::gio::Cancellable::NONE, move |res| {
                        if matches!(res, Ok(1)) {
                            ed2.fields.oc_server_cert.set_text(&pin2);
                            ed2.fetch_groups(); // retry — the pin now lets the probe through
                        }
                    });
                return;
            }

            let groups = probe.groups;
            let combo = &ed.fields.oc_authgroup;
            let current = combo.active_text().map(|s| s.to_string());
            combo.remove_all();
            let mut map = ed.groups.borrow_mut();
            map.clear();
            for (g, otp) in &groups {
                combo.append(Some(g), g);
                map.insert(g.clone(), *otp);
            }
            drop(map);
            // Restore prior selection if still present, else pick the first.
            if let Some(cur) = current.filter(|c| groups.iter().any(|(g, _)| g == c)) {
                combo.set_active_id(Some(&cur));
            } else if let Some((g, _)) = groups.first() {
                combo.set_active_id(Some(g));
            }
            if groups.is_empty() {
                crate::notify::notify("VpncBar", "No groups returned (check the gateway/cert).");
            }
        });
    }

    fn update_info_debug(&self) {
        let p = self.profile.borrow().clone();
        // Live state from the controller's cached map (kept fresh by pidfd
        // watches + the safety-net scan) — no per-second /proc walk here.
        let connected = p.uuid.is_some() && self.app.connected().contains_key(&p.name);

        // Resolve any in-flight Connecting…/Disconnecting… transition: keep it
        // until the real state reaches the target or the 20s deadline lapses.
        let transient = {
            let mut tr = self.transition.borrow_mut();
            match *tr {
                Some((target, started)) if connected != target && started.elapsed().as_secs() < 20 => {
                    Some(if target { "Connecting…" } else { "Disconnecting…" })
                }
                Some(_) => {
                    *tr = None;
                    None
                }
                None => None,
            }
        };

        let conn = self.app.connected();
        let live = conn.get(&p.name).copied();
        let mut s = String::new();
        // Show live details while up (covers Disconnecting…, which keeps the
        // last-known details), a bare verb while Connecting…, else Not connected.
        let show_details = connected && transient != Some("Connecting…");
        match transient {
            Some(verb) => s.push_str(&format!("Status:        {verb}\n")),
            None if connected => s.push_str("Status:        Connected\n"),
            None => s.push_str("Status:        Not connected\n"),
        }
        if show_details {
            let t = crate::tunnel::read_tunnel_info(&p);
            let secs = live.map(|(_, s)| s).unwrap_or(0);
            s.push_str(&format!("Uptime:        {}\n", crate::tunnel::format_elapsed(secs)));
            if let Some(i) = &t.iface {
                s.push_str(&format!("Interface:     {i}\n"));
                if let Some(c) = crate::tunnel::interface_counters(i) {
                    s.push_str(&format!(
                        "Traffic in:    {} ({} pkts)\nTraffic out:   {} ({} pkts)\n",
                        crate::sys::human_bytes(c.rx_bytes), c.rx_pkts,
                        crate::sys::human_bytes(c.tx_bytes), c.tx_pkts
                    ));
                }
            }
            if let Some(v) = &t.internal_ip { s.push_str(&format!("Internal IP:   {v}\n")); }
            if let Some(v) = &t.gateway { s.push_str(&format!("Gateway:       {v}\n")); }
            if let Some(v) = &t.dns { s.push_str(&format!("DNS:           {v}\n")); }
            if let Some(v) = &t.match_domains { s.push_str(&format!("Match domains: {v}\n")); }
            if !t.routes.is_empty() { s.push_str(&format!("Routes:        {}\n", t.routes.join(", "))); }
        }
        match live {
            Some((pid, _)) => s.push_str(&format!("\nCommand (PID {pid}):\n{}\n", crate::backend::command_line(&p))),
            None => s.push_str(&format!("\nCommand:\n{}\n", crate::backend::command_line(&p))),
        }
        self.fields.info_label.set_text(&s);

        // Debug tab. openconnect keeps writing its session to the per-profile log
        // file (inherited stdout/stderr), so we tail that. vpnc abandons those fds
        // on daemonising and logs to syslog instead, so its file is always empty —
        // we pull its lines from the journal, scoped to the live tunnel's PID.
        let view = &self.fields.debug_view;
        if p.is_openconnect() {
            let log = std::fs::read_to_string(crate::model::log_file(&p)).unwrap_or_default();
            set_view_text(view, &log);
        } else if let Some((pid, _)) = live {
            self.last_vpnc_pid.set(Some(pid));
            self.sync_vpnc_log(&p, pid);
        } else if let Some(pid) = self.last_vpnc_pid.take() {
            // First refresh after a drop: one final sync with the last known pid
            // so the teardown lines land in the persisted file. (If a fetch is
            // mid-flight, put the pid back and retry next tick.)
            if self.journal_busy.get() {
                self.last_vpnc_pid.set(Some(pid));
            } else {
                self.sync_vpnc_log(&p, pid);
            }
        } else {
            // Disconnected: keep showing the last session's persisted log until
            // the next connection truncates the boot log and rebuilds it.
            let log = std::fs::read_to_string(crate::model::log_file(&p)).unwrap_or_default();
            if log.trim().is_empty() {
                set_view_text(view, "No session log yet — connect to capture one.");
            } else {
                set_view_text(view, &log);
            }
        }
    }

    /// Busy-guarded async rebuild of the vpnc session log (boot + journal):
    /// persists it to `log_file` (for "Reveal log") and refreshes the Debug
    /// view. Runs while connected, plus once more right after a drop.
    fn sync_vpnc_log(&self, p: &Profile, pid: u32) {
        if self.journal_busy.get() {
            return;
        }
        self.journal_busy.set(true);
        let prof = p.clone();
        let logpath = crate::model::log_file(p);
        let since = self.clear_cutoff.get();
        let (tx, rx) = async_channel::bounded::<Result<String, ()>>(1);
        std::thread::spawn(move || {
            let r = crate::backend::vpnc::session_log(&prof, pid, since);
            if let Ok(text) = &r {
                let _ = std::fs::write(&logpath, text); // persist for the file-based tools
            }
            let _ = tx.send_blocking(r);
        });
        let view = self.fields.debug_view.clone();
        let busy = self.journal_busy.clone();
        glib::spawn_future_local(async move {
            let res = rx.recv().await;
            busy.set(false);
            let text = match res {
                Ok(Ok(t)) if !t.trim().is_empty() => t,
                Ok(Ok(_)) => "No vpnc output yet. At Debug 1 vpnc only logs steady-state \
                              packets; raise Debug to 2 or 3 in Options for the handshake."
                    .to_string(),
                _ => "Can't read the system journal — your user needs to be in the \
                      'systemd-journal' (or 'wheel') group.\n\
                      View manually with:  journalctl -t vpnc"
                    .to_string(),
            };
            set_view_text(&view, &text);
        });
    }

    fn clear_log(&self) {
        let p = self.profile.borrow().clone();
        let _ = std::fs::write(crate::model::log_file(&p), b"");
        if !p.is_openconnect() {
            // vpnc: the runtime lines live in the system journal, which we can't
            // truncate — record a cutoff so only newer lines show from here on,
            // and drop the connect-phase capture too.
            let _ = std::fs::write(crate::model::boot_log_file(&p), b"");
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            self.clear_cutoff.set(Some(now));
        }
        self.update_info_debug();
    }

    fn reveal_log(&self) {
        let p = self.profile.borrow().clone();
        // vpnc's session log is rebuilt (boot + journal); refresh it before
        // opening (synchronous here — it's a one-off click).
        if !p.is_openconnect() {
            if let Some((pid, _)) = self.app.connected().get(&p.name).copied() {
                if let Ok(text) =
                    crate::backend::vpnc::session_log(&p, pid, self.clear_cutoff.get())
                {
                    let _ = std::fs::write(crate::model::log_file(&p), &text);
                }
            }
        }
        let _ = crate::sys::run("/usr/bin/xdg-open", &[&crate::model::log_file(&p).to_string_lossy()], None);
    }

    fn present(self: &Rc<Self>) {
        self.window.present();
    }

    fn is_oc(&self) -> bool {
        self.profile.borrow().is_openconnect()
    }

    fn refresh_connect_button(&self) {
        let p = self.profile.borrow();
        // Only meaningful once saved (has a uuid / persisted name). Not polled:
        // runs at editor open, after save/toggle, and via refresh_open_editors()
        // whenever the controller detects a state change. Reads the controller's
        // cached map — no /proc scan.
        let connected = p.uuid.is_some() && self.app.connected().contains_key(&p.name);
        self.connect_btn.set_label(if connected { "Disconnect" } else { "Connect" });
        self.connect_btn.set_sensitive(p.uuid.is_some() || !self.fields.name.text().is_empty());
    }

    fn toggle_connection(&self) {
        // Persist first so we connect what's on screen (and validate).
        if !self.save() {
            return;
        }
        let p = self.profile.borrow().clone();
        let connected = is_connected(&p);
        let cmd = if connected {
            Cmd::Disconnect(p.name.clone())
        } else {
            Cmd::Connect(p.name.clone())
        };
        // Note the in-flight direction so the Info tab shows Connecting…/Disconnecting…
        *self.transition.borrow_mut() = Some((!connected, std::time::Instant::now()));
        let _ = self.app.sender().send_blocking(cmd);
        self.refresh_connect_button();
        self.update_info_debug();
    }

    /// Required fields per backend — `Some(list)` names the missing ones.
    fn missing_required(&self) -> Option<String> {
        let f = &self.fields;
        let mut missing = Vec::new();
        if f.name.text().trim().is_empty() {
            missing.push("Name");
        }
        if f.gateway.text().trim().is_empty() {
            missing.push("Gateway");
        }
        if f.username.text().trim().is_empty() {
            missing.push("Username");
        }
        if !self.is_oc() && f.id.text().trim().is_empty() {
            missing.push("Group name");
        }
        (!missing.is_empty()).then(|| missing.join(", "))
    }

    /// Gather the form into a Profile, persist it + secrets, and refresh the UI.
    /// Returns false (without saving) if required fields are missing.
    fn save(&self) -> bool {
        if let Some(missing) = self.missing_required() {
            gtk::AlertDialog::builder()
                .message("Missing required fields")
                .detail(format!("Please fill in: {missing}."))
                .modal(true)
                .build()
                .show(Some(&self.window));
            return false;
        }
        let f = &self.fields;
        let mut p = self.profile.borrow().clone();
        let kind = if f_is_oc(&p) { "openconnect" } else { "vpnc" };

        p.kind = Some(kind.to_string());
        p.name = f.name.text().trim().to_string();
        p.gateway = f.gateway.text().trim().to_string();
        p.username = f.username.text().trim().to_string();
        p.dns_match_domains = ne(Some(&f.domains.text()));
        p.client_cert = ne(Some(&f.client_cert.text()));

        if kind == "vpnc" {
            p.id = f.id.text().trim().to_string();
            p.authmode = f.authmode.active_id().map(|s| s.to_string());
            p.ca_file = ne(Some(&f.ca_file.text()));
            p.dh_group = ne(f.dh_group.active_id().as_deref());
            p.pfs = ne(f.pfs.active_id().as_deref());
            p.nat_mode = ne(f.nat_mode.active_id().as_deref());
            p.vendor = ne(f.vendor.active_id().as_deref());
            p.mtu = ne(Some(&f.mtu.text()));
            p.dpd_timeout = ne(Some(&f.dpd_timeout.text()));
            p.debug = f.debug.active_id().map(|s| s.to_string());
            p.enable_weak = Some(f.enable_weak.is_active());
            p.single_des = Some(f.single_des.is_active());
            p.no_encryption = Some(f.no_encryption.is_active());
            p.weak_auth = Some(f.weak_auth.is_active());
        } else {
            // openconnect: id unused; authgroup carries the group.
            p.oc_authgroup = ne(f.oc_authgroup.active_text().as_deref());
            p.oc_server_cert = ne(Some(&f.oc_server_cert.text()));
            p.oc_otp = Some(f.oc_otp.is_active());
            p.oc_protocol = f.oc_protocol.active_id().map(|s| s.to_string());
            p.oc_no_dtls = Some(f.oc_no_dtls.is_active());
            p.oc_dpd = ne(Some(&f.oc_dpd.text()));
            p.oc_mtu = ne(Some(&f.oc_mtu.text()));
            p.oc_reconnect = ne(Some(&f.oc_reconnect.text()));
            p.oc_debug = f.oc_debug.active_id().map(|s| s.to_string());
        }

        // Persist (assigns a uuid if new) then store secrets keyed off it.
        let saved = upsert(p);
        let secret = f.secret.text().to_string();
        let password = f.password.text().to_string();
        if !secret.is_empty() {
            secrets::store(&secrets::kc_service(&saved, "secret"), &saved.id, &secret);
        }
        if !password.is_empty() {
            secrets::store(&secrets::kc_service(&saved, "password"), &saved.username, &password);
        }
        *self.profile.borrow_mut() = saved;

        let _ = self.app.sender().send_blocking(Cmd::Refresh);
        true
    }
}

fn f_is_oc(p: &Profile) -> bool {
    p.is_openconnect()
}

/// (Re)build the notebook pages for the given backend kind.
fn build_pages(notebook: &gtk::Notebook, kind: &str, f: &Fields) {
    while notebook.n_pages() > 0 {
        notebook.remove_page(Some(0));
    }
    // Detach every reused widget from its old page before re-attaching, so a
    // Type switch can't trip "widget already has a parent".
    let widgets: [&gtk::Widget; 32] = [
        f.name.upcast_ref(), f.gateway.upcast_ref(), f.username.upcast_ref(),
        f.password.upcast_ref(), f.domains.upcast_ref(), f.client_cert.upcast_ref(),
        f.id.upcast_ref(), f.secret.upcast_ref(), f.authmode.upcast_ref(),
        f.auth_note.upcast_ref(), f.ca_file.upcast_ref(), f.dh_group.upcast_ref(),
        f.pfs.upcast_ref(), f.nat_mode.upcast_ref(), f.vendor.upcast_ref(),
        f.mtu.upcast_ref(), f.dpd_timeout.upcast_ref(), f.debug.upcast_ref(),
        f.enable_weak.upcast_ref(), f.single_des.upcast_ref(), f.no_encryption.upcast_ref(),
        f.weak_auth.upcast_ref(), f.oc_authgroup.upcast_ref(), f.oc_fetch.upcast_ref(),
        f.oc_server_cert.upcast_ref(), f.oc_otp.upcast_ref(), f.oc_protocol.upcast_ref(),
        f.oc_no_dtls.upcast_ref(), f.oc_dpd.upcast_ref(), f.oc_mtu.upcast_ref(),
        f.oc_reconnect.upcast_ref(), f.oc_debug.upcast_ref(),
    ];
    for w in widgets {
        detach(w);
    }
    let creds = form_grid();
    let mut r = 0;
    add_row(&creds, r, "Name", &f.name);
    r += 1;
    add_row(&creds, r, "Gateway", &f.gateway);
    r += 1;

    if kind == "openconnect" {
        let group_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        group_box.append(&f.oc_authgroup);
        group_box.append(&f.oc_fetch);
        add_row(&creds, r, "Auth group", &group_box);
        r += 1;
        add_row(&creds, r, "Username", &f.username);
        r += 1;
        add_row(&creds, r, "Password", &f.password);
        r += 1;
        add_row(&creds, r, "VPN domains", &f.domains);
        r += 1;
        creds.attach(&f.oc_otp, 1, r, 1, 1);
        r += 1;

        // Cert fields live behind an "Advanced" disclosure (expanded if set).
        let adv_grid = form_grid();
        add_row(&adv_grid, 0, "Server cert", &f.oc_server_cert);
        add_row(&adv_grid, 1, "Client cert", &f.client_cert);
        let advanced = gtk::Expander::new(Some("Advanced"));
        advanced.set_child(Some(&adv_grid));
        advanced.set_expanded(
            !f.oc_server_cert.text().is_empty() || !f.client_cert.text().is_empty(),
        );
        creds.attach(&advanced, 0, r, 2, 1);

        let opts = form_grid();
        let mut o = 0;
        add_row(&opts, o, "Protocol", &*f.oc_protocol);
        o += 1;
        add_row(&opts, o, "DPD (s)", &f.oc_dpd);
        o += 1;
        add_row(&opts, o, "MTU", &f.oc_mtu);
        o += 1;
        add_row(&opts, o, "Reconnect (s)", &f.oc_reconnect);
        o += 1;
        add_row(&opts, o, "Verbosity", &*f.oc_debug);
        o += 1;
        opts.attach(&f.oc_no_dtls, 1, o, 1, 1);

        notebook.append_page(&creds, Some(&gtk::Label::new(Some("Credentials"))));
        notebook.append_page(&opts, Some(&gtk::Label::new(Some("Options"))));
    } else {
        add_row(&creds, r, "Group name", &f.id);
        r += 1;
        add_row(&creds, r, "Group secret", &f.secret);
        r += 1;
        add_row(&creds, r, "Username", &f.username);
        r += 1;
        add_row(&creds, r, "Password", &f.password);
        r += 1;
        add_row(&creds, r, "VPN domains", &f.domains);
        r += 1;
        add_row(&creds, r, "IKE Authmode", &*f.authmode);
        r += 1;
        add_row(&creds, r, "CA file", &f.ca_file);
        r += 1;
        add_row(&creds, r, "Client cert", &f.client_cert);
        r += 1;
        // Cert-support warning spans both columns (hidden unless relevant).
        creds.attach(&f.auth_note, 0, r, 2, 1);

        let opts = form_grid();
        let mut o = 0;
        add_row(&opts, o, "DH Group", &*f.dh_group);
        o += 1;
        add_row(&opts, o, "PFS", &*f.pfs);
        o += 1;
        add_row(&opts, o, "NAT-T Mode", &*f.nat_mode);
        o += 1;
        add_row(&opts, o, "Vendor", &*f.vendor);
        o += 1;
        add_row(&opts, o, "Interface MTU", &f.mtu);
        o += 1;
        add_row(&opts, o, "DPD timeout (s)", &f.dpd_timeout);
        o += 1;
        add_row(&opts, o, "Debug", &*f.debug);
        o += 1;
        opts.attach(&f.enable_weak, 1, o, 1, 1);
        o += 1;
        opts.attach(&f.single_des, 1, o, 1, 1);
        o += 1;
        opts.attach(&f.no_encryption, 1, o, 1, 1);
        o += 1;
        opts.attach(&f.weak_auth, 1, o, 1, 1);

        notebook.append_page(&creds, Some(&gtk::Label::new(Some("Credentials"))));
        notebook.append_page(&opts, Some(&gtk::Label::new(Some("Options"))));
    }

    // Info + Debug tabs (shared by both backends).
    notebook.append_page(&info_page(f), Some(&gtk::Label::new(Some("Info"))));
    notebook.append_page(&debug_page(f), Some(&gtk::Label::new(Some("Debug"))));
    notebook.show();
}

/// Detach a reused widget from any prior parent before re-adding it (pages are
/// rebuilt when the Type changes).
fn detach(w: &impl IsA<gtk::Widget>) {
    if w.as_ref().parent().is_some() {
        w.as_ref().unparent();
    }
}

fn info_page(f: &Fields) -> gtk::Widget {
    detach(&f.info_label);
    f.info_label.set_margin_top(12);
    f.info_label.set_margin_bottom(12);
    f.info_label.set_margin_start(12);
    f.info_label.set_margin_end(12);
    f.info_label.add_css_class("monospace");
    let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
    scroll.set_child(Some(&f.info_label));
    scroll.upcast()
}

fn debug_page(f: &Fields) -> gtk::Widget {
    detach(&f.debug_view);
    detach(&f.debug_clear);
    detach(&f.debug_reveal);

    let vb = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
    scroll.set_child(Some(&f.debug_view));
    vb.append(&scroll);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bar.set_margin_start(8);
    bar.set_margin_end(8);
    bar.set_margin_bottom(8);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bar.append(&spacer);
    bar.append(&f.debug_reveal);
    bar.append(&f.debug_clear);
    vb.append(&bar);
    vb.upcast()
}
