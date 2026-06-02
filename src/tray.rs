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
    // Open the menu on the primary (left) click instead of calling `activate`.
    // Hosts that honour ItemIsMenu will pop the menu for both left and right
    // click, so the main window is reachable via the "Open VpncBar…" entry.
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
        // Fallback for hosts that ignore ItemIsMenu and still send Activate on
        // left click: open the main window (matches the old behaviour).
        Self::send(&self.tx, Cmd::OpenWindow);
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
            for name in &self.profiles {
                let live = self.connected.get(name).copied();
                let label = match live {
                    Some((_, secs)) => format!("✓ {name}    {}", format_elapsed(secs)),
                    None => format!("    {name}"),
                };
                let n = name.clone();
                items.push(
                    StandardItem {
                        label,
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
