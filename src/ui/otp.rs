//! One-time 2FA code prompt for openconnect profiles.
//!
//! The controller calls this synchronously from the glib loop (returning the
//! code before spawning the connect). GTK4 has no `Dialog::run`, so we drive a
//! nested `glib::MainLoop` — the standard replacement — and quit it on response.

use crate::model::Profile;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

pub fn prompt(parent: &gtk::Window, p: &Profile) -> Option<String> {
    let win = gtk::Window::builder()
        .title(format!("One-time code for “{}”", p.name))
        .transient_for(parent)
        .modal(true)
        .resizable(false)
        .build();

    let vb = gtk::Box::new(gtk::Orientation::Vertical, 10);
    vb.set_margin_top(14);
    vb.set_margin_bottom(14);
    vb.set_margin_start(14);
    vb.set_margin_end(14);

    let info = gtk::Label::new(Some("Enter the current 2FA code (e.g. from your authenticator)."));
    info.set_wrap(true);
    vb.append(&info);

    let entry = gtk::Entry::builder().activates_default(true).build();
    entry.set_input_purpose(gtk::InputPurpose::Digits);
    vb.append(&entry);

    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    buttons.append(&cancel);
    buttons.append(&connect);
    vb.append(&buttons);

    win.set_child(Some(&vb));

    let result: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
    let main_loop = glib::MainLoop::new(None, false);

    let finish = {
        let win = win.clone();
        let ml = main_loop.clone();
        move || {
            win.close();
            if ml.is_running() {
                ml.quit();
            }
        }
    };

    {
        let finish = finish.clone();
        cancel.connect_clicked(move |_| finish());
    }
    {
        let result = result.clone();
        let entry = entry.clone();
        let finish = finish.clone();
        connect.connect_clicked(move |_| {
            *result.borrow_mut() = Some(entry.text().to_string());
            finish();
        });
    }
    // Closing the window (Esc / titlebar) cancels.
    {
        let ml = main_loop.clone();
        win.connect_close_request(move |_| {
            if ml.is_running() {
                ml.quit();
            }
            glib::Propagation::Proceed
        });
    }

    win.set_default_widget(Some(&connect));
    win.present();
    main_loop.run(); // blocks until a button/close quits it

    let code = result.borrow().clone();
    code.filter(|s| !s.trim().is_empty())
}
