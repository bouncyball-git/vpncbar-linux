// ComboBoxText is deprecated since GTK 4.10 in favour of DropDown, but is still
// fully functional and far terser for these small enumerated fields. Migrating
// to DropDown+StringList is a cosmetic follow-up.
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
use std::cell::RefCell;
use std::rc::Rc;

thread_local! {
    /// One editor window per profile (keyed by uuid; new profiles share the
    /// "__new__" slot), so re-opening an editor just brings it forward instead
    /// of spawning a duplicate. Mirrors the macOS per-uuid editor registry.
    static OPEN_EDITORS: RefCell<std::collections::HashMap<String, gtk::Window>> =
        RefCell::new(std::collections::HashMap::new());
}

/// Open the editor for an existing profile, or a new one (`None`). If an editor
/// for the same profile is already open, it's presented rather than duplicated.
pub fn open_editor(app: &Rc<App>, parent: &gtk::Window, profile: Option<Profile>) {
    let key = profile
        .as_ref()
        .and_then(|p| p.uuid.clone())
        .unwrap_or_else(|| "__new__".to_string());

    if let Some(win) = OPEN_EDITORS.with(|m| m.borrow().get(&key).cloned()) {
        win.present();
        return;
    }

    let ed = Editor::new(app, parent, profile);
    OPEN_EDITORS.with(|m| m.borrow_mut().insert(key.clone(), ed.window.clone()));
    // Drop the registry entry when the window closes so it can be reopened.
    ed.window.connect_close_request(move |_| {
        OPEN_EDITORS.with(|m| {
            m.borrow_mut().remove(&key);
        });
        glib::Propagation::Proceed
    });
    ed.present();
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
    authmode: gtk::ComboBoxText,
    /// Shown when cert/hybrid is chosen on a vpnc built without GnuTLS.
    auth_note: gtk::Label,
    ca_file: gtk::Entry,
    dh_group: gtk::ComboBoxText,
    pfs: gtk::ComboBoxText,
    nat_mode: gtk::ComboBoxText,
    vendor: gtk::ComboBoxText,
    mtu: gtk::Entry,
    dpd_timeout: gtk::Entry,
    debug: gtk::ComboBoxText,
    enable_weak: gtk::CheckButton,
    single_des: gtk::CheckButton,
    no_encryption: gtk::CheckButton,
    weak_auth: gtk::CheckButton,
    // openconnect
    oc_authgroup: gtk::ComboBoxText,
    oc_server_cert: gtk::Entry,
    oc_otp: gtk::CheckButton,
    oc_protocol: gtk::ComboBoxText,
    oc_no_dtls: gtk::CheckButton,
    oc_dpd: gtk::Entry,
    oc_mtu: gtk::Entry,
    oc_reconnect: gtk::Entry,
    oc_debug: gtk::ComboBoxText,
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

fn combo(items: &[(&str, &str)], active: Option<&str>) -> gtk::ComboBoxText {
    let c = gtk::ComboBoxText::new();
    for (id, label) in items {
        c.append(Some(id), label);
    }
    c.set_active_id(active.or(items.first().map(|(id, _)| *id)));
    c
}

/// A dropdown whose first entry is an empty "(default)" choice, so leaving it
/// alone omits the directive (matching the free-text "blank = backend default"
/// behaviour) while still offering the valid values. `active` selects an
/// existing value, else "(default)".
fn combo_default(items: &[&str], active: Option<&str>) -> gtk::ComboBoxText {
    let c = gtk::ComboBoxText::new();
    c.append(Some(""), "(default)");
    for it in items {
        c.append(Some(it), it);
    }
    c.set_active_id(active.filter(|s| !s.is_empty()).or(Some("")));
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
        type_bar.set_margin_start(12);
        type_bar.set_margin_end(12);
        let type_label = gtk::Label::new(Some("Type:"));
        let type_combo = combo(
            &[("vpnc", "Cisco IPSec (vpnc)"), ("openconnect", "AnyConnect (openconnect)")],
            Some(kind),
        );
        type_combo.set_sensitive(is_new);
        type_bar.append(&type_label);
        type_bar.append(&type_combo);
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
            let groups = crate::backend::openconnect::group_list(&server, cert.as_deref());
            let _ = tx.send_blocking(groups);
        });

        let ed = self.clone();
        glib::spawn_future_local(async move {
            let groups = rx.recv().await.unwrap_or_default();
            ed.fields.oc_fetch.set_label("Fetch groups");
            ed.fields.oc_fetch.set_sensitive(true);

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
        // Info: live tunnel state (only meaningful when connected).
        let connected = p.uuid.is_some() && is_connected(&p);

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

        // Debug: tail of the per-session log.
        let log = std::fs::read_to_string(crate::model::log_file(&p)).unwrap_or_default();
        let buf = self.fields.debug_view.buffer();
        if buf.text(&buf.start_iter(), &buf.end_iter(), false) != log {
            buf.set_text(&log);
        }
    }

    fn clear_log(&self) {
        let p = self.profile.borrow().clone();
        let _ = std::fs::write(crate::model::log_file(&p), b"");
        self.update_info_debug();
    }

    fn reveal_log(&self) {
        let p = self.profile.borrow().clone();
        let dir = crate::model::run_dir();
        let _ = dir; // ensure path computed
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
        // Only meaningful once saved (has a uuid / persisted name).
        let connected = p.uuid.is_some() && is_connected(&p);
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
        add_row(&opts, o, "Protocol", &f.oc_protocol);
        o += 1;
        add_row(&opts, o, "DPD (s)", &f.oc_dpd);
        o += 1;
        add_row(&opts, o, "MTU", &f.oc_mtu);
        o += 1;
        add_row(&opts, o, "Reconnect (s)", &f.oc_reconnect);
        o += 1;
        add_row(&opts, o, "Verbosity", &f.oc_debug);
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
        add_row(&creds, r, "IKE Authmode", &f.authmode);
        r += 1;
        add_row(&creds, r, "CA file", &f.ca_file);
        r += 1;
        add_row(&creds, r, "Client cert", &f.client_cert);
        r += 1;
        // Cert-support warning spans both columns (hidden unless relevant).
        creds.attach(&f.auth_note, 0, r, 2, 1);

        let opts = form_grid();
        let mut o = 0;
        add_row(&opts, o, "DH Group", &f.dh_group);
        o += 1;
        add_row(&opts, o, "PFS", &f.pfs);
        o += 1;
        add_row(&opts, o, "NAT-T Mode", &f.nat_mode);
        o += 1;
        add_row(&opts, o, "Vendor", &f.vendor);
        o += 1;
        add_row(&opts, o, "Interface MTU", &f.mtu);
        o += 1;
        add_row(&opts, o, "DPD timeout (s)", &f.dpd_timeout);
        o += 1;
        add_row(&opts, o, "Debug", &f.debug);
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
