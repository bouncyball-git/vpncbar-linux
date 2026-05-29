//! Desktop notifications via org.freedesktop.Notifications (notify-rust),
//! replacing the macOS UserNotifications usage.

use notify_rust::Notification;

pub fn notify(summary: &str, body: &str) {
    let _ = Notification::new()
        .summary(summary)
        .body(body)
        .icon("network-vpn")
        .appname("VpncBar")
        .show();
}
