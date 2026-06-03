//! GTK4 UI: the main window (rich profile list with live timers + per-row
//! edit), the profile editor, Info/Debug tabs, About, and the OTP prompt.
//!
//! The tray opens these via the controller's `UiHooks`. Windows are created once
//! at startup and reused (hidden when closed), so the tray-only app keeps running.

mod about;
mod editor;
mod otp;
mod window;

use crate::app::{App, UiHooks};
use std::rc::Rc;

pub use editor::open_editor;

/// Build the windows and wire the controller's UI hooks.
pub fn install_hooks(app: &Rc<App>, application: &gtk::Application) {
    let win = window::MainWindow::new(app, application);

    let w_open = win.clone();
    let w_refresh = win.clone();
    let w_about = win.clone();
    let w_otp = win.clone();
    let app_about = app.clone();

    app.set_hooks(UiHooks {
        open_window: Some(Box::new(move || w_open.present())),
        about: Some(Box::new(move || about::show(&w_about.gtk_window(), &app_about))),
        request_otp: Some(Box::new(move |p| otp::prompt(&w_otp.gtk_window(), p))),
        // State changed (connect/disconnect/drop): redraw the window AND push
        // the new state into any open editors (Connect/Disconnect button).
        on_refresh: Some(Box::new(move || {
            w_refresh.refresh();
            editor::refresh_open_editors();
        })),
    });

    // Debug aid for development screenshots: VPNCBAR_AUTOEDIT=new|<profile name>.
    if let Ok(which) = std::env::var("VPNCBAR_AUTOEDIT") {
        win.present();
        let p = if which == "new" {
            None
        } else {
            app.profiles().into_iter().find(|p| p.name == which)
        };
        open_editor(app, &win.gtk_window(), p);
    }
}
