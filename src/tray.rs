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
    fn id(&self) -> String {
        "vpncbar".into()
    }

    fn title(&self) -> String {
        "VpncBar".into()
    }

    fn icon_name(&self) -> String {
        if self.connected.is_empty() {
            "network-vpn-disconnected-symbolic".into()
        } else {
            "network-vpn-symbolic".into()
        }
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
            icon_name: self.icon_name(),
            icon_pixmap: vec![],
        }
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        // Primary (left) click on the icon opens the main window.
        Self::send(&self.tx, Cmd::OpenWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items: Vec<MenuItem<Self>> = Vec::new();

        if self.profiles.is_empty() {
            items.push(
                StandardItem {
                    label: "No VPNs — use Manage VPNs…".into(),
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
                label: "Open VpncBar…".into(),
                activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::OpenWindow)),
                ..Default::default()
            }
            .into(),
        );
        items.push(
            StandardItem {
                label: "About VpncBar".into(),
                activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::About)),
                ..Default::default()
            }
            .into(),
        );
        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit VpncBar".into(),
                activate: Box::new(|t: &mut Self| Self::send(&t.tx, Cmd::Quit)),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}
