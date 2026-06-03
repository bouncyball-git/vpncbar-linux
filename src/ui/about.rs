//! About window — version, backend versions, project link, and an in-app
//! uninstaller (mirrors the macOS About panel).

use crate::app::App;
use crate::sys;
use gtk::prelude::*;
use std::rc::Rc;

const REPO_URL: &str = "https://github.com/bouncyball-git/vpncbar-linux";

pub fn show(parent: &gtk::Window, app: &Rc<App>) {
    let win = gtk::Window::builder()
        .title("About VpncBar")
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .default_width(400)
        .build();

    let vb = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vb.set_margin_top(20);
    vb.set_margin_bottom(16);
    vb.set_margin_start(20);
    vb.set_margin_end(20);

    // The closed-lock application icon, rendered from the bundled SVG so it
    // shows even when running uninstalled (falls back to the themed name).
    let logo = match crate::tray_icon::closed_lock_texture(64, (0.92, 0.92, 0.92)) {
        Some(tex) => gtk::Image::from_paintable(Some(&tex)),
        None => gtk::Image::from_icon_name("io.github.vpncbar"),
    };
    logo.set_pixel_size(64);
    vb.append(&logo);

    let name = gtk::Label::new(None);
    name.set_markup("<span size='x-large' weight='bold'>VpncBar</span>");
    vb.append(&name);

    let ver = gtk::Label::new(Some(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
    ver.add_css_class("dim-label");
    vb.append(&ver);

    let desc = gtk::Label::new(Some(
        "A tray front-end for vpnc (Cisco IPSec) and openconnect (AnyConnect SSL).",
    ));
    desc.set_wrap(true);
    desc.set_justify(gtk::Justification::Center);
    vb.append(&desc);

    // Backend versions (and licences), matching the macOS About panel.
    let vpnc_line = sys::tool_version(sys::VPNC)
        .map(|v| format!("{v} · GPLv2"))
        .unwrap_or_else(|| "vpnc: not installed".into());
    let oc_line = match sys::openconnect_path() {
        Some(p) => sys::tool_version(p)
            .map(|v| format!("{v} · LGPLv2.1"))
            .unwrap_or_else(|| "openconnect: installed".into()),
        None => "openconnect: not installed".into(),
    };
    let backends = gtk::Label::new(Some(&format!("{vpnc_line}\n{oc_line}")));
    backends.add_css_class("dim-label");
    backends.add_css_class("caption");
    backends.set_justify(gtk::Justification::Center);
    vb.append(&backends);

    let link = gtk::LinkButton::with_label(REPO_URL, "github.com/bouncyball-git/vpncbar-linux");
    link.set_halign(gtk::Align::Center); // natural width, not stretched across the box
    vb.append(&link);

    // Stacked buttons: Close sits under Uninstall (equal width, centred).
    let buttons = gtk::Box::new(gtk::Orientation::Vertical, 8);
    buttons.set_halign(gtk::Align::Center);
    buttons.set_margin_top(8);
    let uninstall = gtk::Button::with_label("Uninstall VpncBar…");
    let close = gtk::Button::with_label("Close");
    close.set_halign(gtk::Align::Center); // natural size, don't stretch to Uninstall's width
    buttons.append(&uninstall);
    buttons.append(&close);
    vb.append(&buttons);

    win.set_child(Some(&vb));

    {
        let win = win.clone();
        close.connect_clicked(move |_| win.close());
    }
    {
        let app = app.clone();
        let win = win.clone();
        uninstall.connect_clicked(move |_| confirm_uninstall(&win, &app));
    }

    win.present();
    // The link is the first focusable widget and would grab initial focus,
    // drawing a focus ring around it. Start with nothing focused instead.
    gtk::prelude::GtkWindowExt::set_focus(&win, None::<&gtk::Widget>);
}

/// Ask, then disconnect all tunnels and remove the installed files (the system
/// files via a polkit prompt). Profiles + stored secrets are kept.
fn confirm_uninstall(parent: &gtk::Window, app: &Rc<App>) {
    let dialog = gtk::AlertDialog::builder()
        .message("Uninstall VpncBar?")
        .detail(
            "This disconnects all tunnels and removes the installed binary, helper script, \
             polkit rule and launcher. Your saved profiles and stored secrets are kept.",
        )
        .buttons(["Cancel", "Uninstall"])
        .cancel_button(0)
        .default_button(0)
        .modal(true)
        .build();

    let app = app.clone();
    let parent_cb = parent.clone();
    dialog.choose(Some(parent), gtk::gio::Cancellable::NONE, move |res| {
        let parent = &parent_cb;
        if res != Ok(1) {
            return;
        }
        // Bring tunnels down while the validated helper is still installed.
        app.disconnect_all_sync();
        // User-level autostart entry needs no privilege.
        if let Some(cfg) = dirs::config_dir() {
            let _ = std::fs::remove_file(cfg.join("autostart/io.github.vpncbar.desktop"));
            let _ = std::fs::remove_file(cfg.join("autostart/vpncbar.desktop"));
        }
        // System files: one privileged shell (prompts for admin auth — /bin/sh
        // isn't covered by the passwordless rule, which is intentional).
        let script = "rm -f /usr/bin/vpncbar \
             /usr/lib/vpncbar/vpncbar-script \
             /usr/lib/vpncbar/vpncbar-disconnect \
             /etc/polkit-1/rules.d/10-vpncbar.rules \
             /usr/share/applications/io.github.vpncbar.desktop \
             /usr/share/applications/vpncbar.desktop \
             /usr/share/icons/hicolor/scalable/apps/io.github.vpncbar.svg \
             /usr/share/icons/hicolor/scalable/apps/vpncbar.svg; \
             rmdir /usr/lib/vpncbar 2>/dev/null; exit 0";
        let r = crate::privilege::run_root("/bin/sh", &["-c", script], None);
        if r.ok() {
            std::process::exit(0);
        }
        let detail = if crate::privilege::was_dismissed(&r) {
            "Authorization was cancelled — nothing was removed.".to_string()
        } else {
            format!("Removal failed (status {}):\n{}", r.status, if r.err.is_empty() { r.out } else { r.err })
        };
        gtk::AlertDialog::builder()
            .message("Uninstall failed")
            .detail(detail)
            .modal(true)
            .build()
            .show(Some(parent));
    });
}
