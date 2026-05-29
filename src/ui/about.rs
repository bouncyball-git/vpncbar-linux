//! About dialog.

use gtk::prelude::*;

pub fn show(parent: &gtk::Window) {
    let about = gtk::AboutDialog::builder()
        .program_name("VpncBar")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("A tray front-end for vpnc (Cisco IPSec) and openconnect (AnyConnect SSL).")
        .license_type(gtk::License::Gpl20)
        .logo_icon_name("network-vpn")
        .modal(true)
        .transient_for(parent)
        .build();
    about.present();
}
