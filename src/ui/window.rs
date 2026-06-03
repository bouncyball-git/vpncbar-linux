//! The main "VpncBar" window: the rich profile list that replaces the macOS
//! menu rows. Each row shows a ✓ when connected and a live, per-second elapsed
//! timer. The rows are non-interactive (no hover); per-row buttons connect/
//! disconnect (lightning), edit, and remove the profile.

use crate::app::{App, Cmd};
use crate::model::{remove_profile, Profile};
use crate::tunnel::format_elapsed;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

/// A connected row's live-timer state: elapsed at last snapshot + when we took it.
struct LiveRow {
    elapsed_label: gtk::Label,
    base_secs: u64,
    snapped: Instant,
}

pub struct MainWindowInner {
    window: gtk::ApplicationWindow,
    list: gtk::ListBox,
    disc_all: gtk::Button,
    app: Rc<App>,
    live: RefCell<Vec<LiveRow>>,
}

#[derive(Clone)]
pub struct MainWindow(Rc<MainWindowInner>);

impl MainWindow {
    pub fn new(app: &Rc<App>, application: &gtk::Application) -> MainWindow {
        let window = gtk::ApplicationWindow::builder()
            .application(application)
            .title("VpncBar")
            .default_width(420)
            .default_height(460)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let scroll = gtk::ScrolledWindow::builder().vexpand(true).build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("boxed-list");
        list.set_margin_top(8);
        list.set_margin_bottom(8);
        list.set_margin_start(8);
        list.set_margin_end(8);
        scroll.set_child(Some(&list));
        root.append(&scroll);

        // Bottom action bar: Add / Import / Disconnect All.
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_top(6);
        bar.set_margin_bottom(8);
        bar.set_margin_start(8);
        bar.set_margin_end(8);
        let add = gtk::Button::with_label("Add VPN…");
        let import = gtk::Button::with_label("Import…");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let disc_all = gtk::Button::with_label("Disconnect All");
        bar.append(&add);
        bar.append(&import);
        bar.append(&spacer);
        bar.append(&disc_all);
        root.append(&bar);

        window.set_child(Some(&root));

        // Closing the window just hides it (the tray keeps the app alive).
        window.connect_close_request(|w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });

        let inner = Rc::new(MainWindowInner {
            window: window.clone(),
            list,
            disc_all: disc_all.clone(),
            app: app.clone(),
            live: RefCell::new(Vec::new()),
        });
        let me = MainWindow(inner);

        // Wire the action bar.
        {
            let me2 = me.clone();
            add.connect_clicked(move |_| me2.0.app_open_editor(None));
        }
        {
            let me2 = me.clone();
            import.connect_clicked(move |_| me2.import_dialog());
        }
        {
            let app = app.clone();
            disc_all.connect_clicked(move |_| {
                let _ = app.sender().send_blocking(Cmd::DisconnectAll);
            });
        }

        // Tick the elapsed labels every second while the window is shown.
        {
            let me2 = me.clone();
            glib::timeout_add_seconds_local(1, move || {
                if me2.0.window.is_visible() {
                    me2.tick();
                }
                glib::ControlFlow::Continue
            });
        }

        me.rebuild();
        me
    }

    pub fn gtk_window(&self) -> gtk::Window {
        self.0.window.clone().upcast()
    }

    pub fn present(&self) {
        self.rebuild();
        self.0.window.present();
    }

    /// Re-read state and rebuild the list (called on controller refresh).
    pub fn refresh(&self) {
        if self.0.window.is_visible() {
            self.rebuild();
        }
    }

    /// Update just the elapsed labels (cheap, every second).
    fn tick(&self) {
        for r in self.0.live.borrow().iter() {
            let secs = r.base_secs + r.snapped.elapsed().as_secs();
            r.elapsed_label.set_text(&format_elapsed(secs));
        }
    }

    fn rebuild(&self) {
        let inner = &self.0;
        // Clear existing rows.
        while let Some(child) = inner.list.first_child() {
            inner.list.remove(&child);
        }
        inner.live.borrow_mut().clear();

        let profiles = inner.app.profiles();
        let connected = inner.app.connected();

        // "Disconnect All" only when at least one tunnel is up (matches macOS).
        inner.disc_all.set_visible(!connected.is_empty());

        if profiles.is_empty() {
            let row = gtk::ListBoxRow::new();
            row.set_selectable(false);
            row.set_activatable(false);
            let l = gtk::Label::new(Some("No VPNs yet — use “Add VPN…”."));
            l.set_margin_top(16);
            l.set_margin_bottom(16);
            l.add_css_class("dim-label");
            row.set_child(Some(&l));
            inner.list.append(&row);
            return;
        }

        let now = Instant::now();
        for p in &profiles {
            let live = connected.get(&p.name).copied();
            let row = self.build_row(p, live, now);
            inner.list.append(&row);
        }
    }

    fn build_row(&self, p: &Profile, live: Option<(u32, u64)>, now: Instant) -> gtk::ListBoxRow {
        let inner = &self.0;
        let row = gtk::ListBoxRow::new();
        row.set_activatable(false); // no row hover; actions live on the buttons

        let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        hbox.set_margin_top(6);
        hbox.set_margin_bottom(6);
        hbox.set_margin_start(10);
        hbox.set_margin_end(10);

        // Connected check.
        let check = gtk::Image::from_icon_name(if live.is_some() {
            "emblem-ok-symbolic"
        } else {
            "" // empty keeps spacing consistent
        });
        check.set_pixel_size(16);
        hbox.append(&check);

        // Name + backend subtitle.
        let vb = gtk::Box::new(gtk::Orientation::Vertical, 0);
        vb.set_hexpand(true);
        let name = gtk::Label::new(Some(&p.name));
        name.set_xalign(0.0);
        let sub = gtk::Label::new(Some(if p.is_openconnect() {
            "AnyConnect (openconnect)"
        } else {
            "Cisco IPSec (vpnc)"
        }));
        sub.set_xalign(0.0);
        sub.add_css_class("dim-label");
        sub.add_css_class("caption");
        vb.append(&name);
        vb.append(&sub);
        hbox.append(&vb);

        // Live elapsed (monospaced so digits don't jitter).
        let elapsed = gtk::Label::new(None);
        elapsed.add_css_class("numeric");
        elapsed.add_css_class("dim-label");
        if let Some((_, secs)) = live {
            elapsed.set_text(&format_elapsed(secs));
            inner.live.borrow_mut().push(LiveRow {
                elapsed_label: elapsed.clone(),
                base_secs: secs,
                snapped: now,
            });
        }
        hbox.append(&elapsed);

        // Action buttons: connect/disconnect (lightning), edit, remove. Flat so
        // they stay quiet until hovered; the row itself is non-activatable, so
        // only these buttons highlight on hover (not the whole row).
        let connected = live.is_some();

        let connect = gtk::Button::new();
        connect.set_has_frame(false);
        connect.set_tooltip_text(Some(if connected { "Disconnect" } else { "Connect" }));
        // Self-rendered bolt in a light (near-white) fill so it stands out on the
        // dark list. Slashed bolt = connected → click disconnects.
        let bolt = gtk::Image::new();
        if let Some(tex) = crate::tray_icon::bolt_texture(20, (0.95, 0.95, 0.95), connected) {
            bolt.set_paintable(Some(&tex));
        }
        connect.set_child(Some(&bolt));

        let edit = gtk::Button::from_icon_name("document-edit-symbolic");
        edit.set_has_frame(false);
        edit.set_tooltip_text(Some("Edit"));
        let del = gtk::Button::from_icon_name("user-trash-symbolic");
        del.set_has_frame(false);
        del.set_tooltip_text(Some("Remove"));
        hbox.append(&connect);
        hbox.append(&edit);
        hbox.append(&del);

        row.set_child(Some(&hbox));

        {
            let app = inner.app.clone();
            let name = p.name.clone();
            connect.connect_clicked(move |_| {
                let cmd = if connected {
                    Cmd::Disconnect(name.clone())
                } else {
                    Cmd::Connect(name.clone())
                };
                let _ = app.sender().send_blocking(cmd);
            });
        }
        {
            let me = self.clone();
            let p = p.clone();
            edit.connect_clicked(move |_| me.0.app_open_editor(Some(p.clone())));
        }
        {
            let me = self.clone();
            let p = p.clone();
            del.connect_clicked(move |_| me.confirm_remove(p.clone()));
        }
        row
    }

    fn confirm_remove(&self, p: Profile) {
        let dialog = gtk::AlertDialog::builder()
            .message(format!("Remove “{}”?", p.name))
            .detail("This deletes the profile and its stored secrets.")
            .buttons(["Cancel", "Remove"])
            .cancel_button(0)
            .default_button(0)
            .modal(true)
            .build();
        let app = self.0.app.clone();
        dialog.choose(Some(&self.gtk_window()), gtk::gio::Cancellable::NONE, move |res| {
            if res == Ok(1) {
                // Drop secrets too (mirrors macOS removeProfile).
                crate::secrets::delete(&crate::secrets::kc_service(&p, "secret"));
                crate::secrets::delete(&crate::secrets::kc_service(&p, "password"));
                remove_profile(&p);
                let _ = app.sender().send_blocking(Cmd::Refresh);
            }
        });
    }

    fn import_dialog(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Cisco .pcf / vpnc .conf"));
        filter.add_pattern("*.pcf");
        filter.add_pattern("*.conf");
        let filters = gtk::gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);

        let dialog = gtk::FileDialog::builder()
            .title("Import VPN config")
            .filters(&filters)
            .modal(true)
            .build();

        let me = self.clone();
        dialog.open(Some(&self.gtk_window()), gtk::gio::Cancellable::NONE, move |res| {
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            me.import_file(&path.to_string_lossy());
        });
    }

    /// Parse an imported config, persist it + any decoded secrets, then open the
    /// editor so the user can review/fill the rest.
    fn import_file(&self, path: &str) {
        let Some(parsed) = crate::config_import::parse_config_file(path) else {
            let d = gtk::AlertDialog::builder()
                .message("Couldn't import that file")
                .detail("It doesn't look like a Cisco .pcf or vpnc .conf file.")
                .modal(true)
                .build();
            d.show(Some(&self.gtk_window()));
            return;
        };
        let saved = crate::model::upsert(parsed.profile);
        if let Some(s) = &parsed.secret {
            crate::secrets::store(&crate::secrets::kc_service(&saved, "secret"), &saved.id, s);
        }
        if let Some(pw) = &parsed.password {
            crate::secrets::store(&crate::secrets::kc_service(&saved, "password"), &saved.username, pw);
        }
        let _ = self.0.app.sender().send_blocking(Cmd::Refresh);
        self.0.app_open_editor(Some(saved));
    }
}

impl MainWindowInner {
    fn app_open_editor(&self, p: Option<Profile>) {
        crate::ui::open_editor(&self.app, &self.window.clone().upcast(), p);
    }
}
