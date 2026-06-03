//! StatusNotifierItem tray (via ksni). Renders the menu from a snapshot of the
//! profile list + connected state, and routes activations to the controller as
//! `Cmd`s over a channel (so backend work never blocks the tray's D-Bus thread).
//!
//! Per the hybrid UX decision: the tray menu does quick connect/disconnect; the
//! rich macOS-style list (live timers, per-row edit, Info/Debug) lives in the
//! GTK window opened via "Open VpncBar…".

use crate::app::Cmd;
use crate::tunnel::format_elapsed;
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem};
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
    // Left-click calls `activate()` (opens the VPN Manager window); right-click
    // shows the menu (host-handled). Leaving this false is what splits the two
    // buttons — setting it true would route left-click to the menu as well.
    const MENU_ON_ACTIVATE: bool = false;

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
        // Primary (left) click: open the VPN Manager window.
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
            // Use CheckmarkItem so the HOST draws the connected tick in its own
            // gutter column (left of the label). That keeps every VPN name
            // vertically aligned — checked or not — and aligned with the other
            // menu entries, which a text "✓ " prefix can't do (it's part of the
            // label, and the glyph isn't the same width as the spaces we'd pad
            // unchecked rows with). The label carries only the name + elapsed.
            for name in &self.profiles {
                let live = self.connected.get(name).copied();
                let label = match live {
                    Some((_, secs)) => format!("{name} ({})", format_elapsed(secs)),
                    None => name.clone(),
                };
                let n = name.clone();
                items.push(
                    CheckmarkItem {
                        label,
                        checked: live.is_some(),
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
