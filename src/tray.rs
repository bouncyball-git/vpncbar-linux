//! StatusNotifierItem tray (via ksni). Renders the menu from a snapshot of the
//! profile list + connected state, and routes activations to the controller as
//! `Cmd`s over a channel (so backend work never blocks the tray's D-Bus thread).
//!
//! Per the hybrid UX decision: the tray menu does quick connect/disconnect; the
//! rich macOS-style list (live timers, per-row edit, Info/Debug) lives in the
//! GTK window opened via "Open VpncBar…".

use crate::app::Cmd;
use crate::tunnel::format_elapsed;
use ksni::menu::{MenuItem, StandardItem};
use std::collections::HashMap;

pub struct Tray {
    /// Snapshot used to render the menu; refreshed via `Handle::update`.
    pub profiles: Vec<String>,
    pub connected: HashMap<String, (u32, u64)>,
    pub tx: async_channel::Sender<Cmd>,
}

impl Tray {
    fn send(tx: &async_channel::Sender<Cmd>, cmd: Cmd) {
        // Tray thread → controller. Blocking send is fine (channel is unbounded).
        let _ = tx.send_blocking(cmd);
    }
}

impl ksni::Tray for Tray {
    // Route left-click to the menu too, so it behaves the same as right-click
    // (ItemIsMenu). Hosts that honour it won't call `activate()`; the one below
    // stays as a fallback for hosts that ignore the hint.
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "vpncbar".into()
    }

    fn title(&self) -> String {
        "VpncBar".into()
    }

    // No icon_name: we ship our own padlock pixmap (icon_pixmap) so the glyph is
    // identical on every desktop. An empty IconName makes hosts use the pixmap.
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        crate::tray_icon::padlock_set(!self.connected.is_empty())
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        let n = self.connected.len();
        ksni::ToolTip {
            title: "VpncBar".into(),
            description: if n == 0 {
                "No tunnels up".into()
            } else {
                format!("{n} tunnel{} up", if n == 1 { "" } else { "s" })
            },
            icon_name: String::new(),
            icon_pixmap: crate::tray_icon::padlock_set(!self.connected.is_empty()),
        }
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        // Fallback only: with MENU_ON_ACTIVATE, hosts that honour ItemIsMenu show
        // the menu on left-click and never call this. Hosts that ignore it land
        // here — open the VPN Manager window rather than doing nothing.
        Self::send(&self.tx, Cmd::OpenWindow);
    }

    // Called on the ksni service thread when the StatusNotifierWatcher can't be
    // reached. We return `true` to keep the service alive so the icon attaches
    // automatically if a host appears later. `Error` means no SNI support at all
    // (e.g. GNOME without the AppIndicator extension at launch) — fall back to a
    // window. `No` is the transient case (e.g. a GNOME-on-Xorg shell restart),
    // which usually recovers on its own, so we just log it to avoid window churn.
    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        match reason {
            ksni::OfflineReason::Error(e) => {
                log::warn!("status-tray host unavailable ({e}); opening window fallback");
                Self::send(&self.tx, Cmd::TrayUnavailable);
            }
            other => log::info!("status-tray watcher offline ({other:?}); awaiting recovery"),
        }
        true
    }

    fn watcher_online(&self) {
        log::info!("status-tray watcher back online");
        Self::send(&self.tx, Cmd::TrayRestored);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();

        if self.profiles.is_empty() {
            items.push(
                StandardItem {
                    label: "No VPNs".into(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            );
        } else {
            // Show the connected tick as the item's ICON (a themed checkmark in
            // the host's gutter), NOT a CheckmarkItem. A checkmark item makes KDE
            // optimistically toggle the box on click; when the async connect then
            // fails or its OTP is cancelled, the box stays ticked with no model
            // change for us to push back — so it desyncs from reality. An icon has
            // no toggle semantics: it's drawn purely from our `connected` snapshot,
            // so it's always correct. The gutter still aligns every name (the host
            // reserves the icon column menu-wide).
            for name in &self.profiles {
                let live = self.connected.get(name).copied();
                let label = match live {
                    Some((_, secs)) => format!("{name} ({})", format_elapsed(secs)),
                    None => name.clone(),
                };
                let n = name.clone();
                items.push(
                    StandardItem {
                        label,
                        icon_name: if live.is_some() { "checkmark".into() } else { String::new() },
                        activate: Box::new(move |t: &mut Self| {
                            let connected = t.connected.contains_key(&n);
                            let cmd = if connected {
                                Cmd::Disconnect(n.clone())
                            } else {
                                Cmd::Connect(n.clone())
                            };
                            Self::send(&t.tx, cmd);
                        }),
                        ..Default::default()
                    }
                    .into(),
                );
            }
        }

        items.push(MenuItem::Separator);

        if !self.connected.is_empty() {
            items.push(
                StandardItem {
                    label: "Disconnect All".into(),
                    activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::DisconnectAll)),
                    ..Default::default()
                }
                .into(),
            );
        }
        items.push(
            StandardItem {
                label: "VPN Manager".into(),
                activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::OpenWindow)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "About".into(),
                activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::About)),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}
