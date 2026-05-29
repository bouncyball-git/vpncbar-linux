# VpncBar (Linux / Wayland)

A native **GTK4 + StatusNotifierItem** tray front-end for two VPN backends, ported
from the macOS VpncBar:

- **`vpnc`** — Cisco IPSec (IKEv1 + XAUTH).
- **`openconnect`** — Cisco **AnyConnect** SSL (and compatible), with guided group
  setup and per-group 2FA detection.

It lives in the system tray, manages profiles, stores secrets in the **Secret
Service** (GNOME Keyring / KWallet), and brings tunnels up and down with a click.
Unlike the macOS build it does **not** vendor `vpnc` — it uses your distro's
`vpnc`/`openconnect` and the native `tun` driver.

## How it differs from the macOS version

| Area | macOS | Linux |
|------|-------|-------|
| UI toolkit | AppKit (menu-bar) | GTK4 + SNI tray + a main window |
| Tray menu | custom-drawn rows | SNI menu (quick connect/disconnect) |
| Rich list (live timers, per-row edit, Info/Debug) | in the menu | in the GTK window |
| Secrets | Keychain (`security`) | Secret Service (`secret-tool`) |
| Notifications | UserNotifications | `org.freedesktop.Notifications` |
| Privilege | `sudo` + sudoers | **polkit** (`pkexec`) + a passwordless rule |
| VPN backends | vendored static `vpnc` | distro `vpnc` / `openconnect` |
| Scoped DNS | scutil `State:/Network` | systemd-resolved via the vpnc-script |

> **Wayland note:** the tray uses the StatusNotifierItem spec. It works natively on
> **KDE Plasma** and on **Sway/Hyprland via Waybar**; on **GNOME** you need the
> *AppIndicator and KStatusNotifierItem Support* extension. Wayland does not let a
> client position a popup at the tray icon, which is why the rich list is a normal
> window rather than a drop-down menu.

## Requirements

- **Rust** 1.83+ (build only).
- **GTK 4**, **libdbus** (build + run).
- **vpnc** (Cisco IPSec). **openconnect** only for AnyConnect profiles.
- **polkit** (`pkexec`) and a **Secret Service** provider (gnome-keyring or KWallet).

On Arch/Manjaro:

```sh
sudo pacman -S --needed rust gtk4 vpnc openconnect libsecret polkit
```

## Build & install

```sh
./install.sh        # builds release, installs binary + script + polkit rule + .desktop
                    # and adds you to the passwordless 'vpncbar' group
```

Log out/in (or `newgrp vpncbar`) once, so connecting/disconnecting won't prompt for
a password. Then launch **VpncBar** from your app menu, or run `vpncbar`. To start
it on login: `cp /usr/share/applications/vpncbar.desktop ~/.config/autostart/`.

Uninstall with `./uninstall.sh` (keeps your profiles + secrets).

### Running uninstalled (development)

`cargo run` works without installing: the tray and UI run, and connecting falls
back to the distro `vpnc-script` and a polkit prompt per action. Headless
subcommands help exercise the core:

```sh
cargo run -- list
cargo run -- config <name>          # show the generated vpnc.conf / openconnect argv
cargo run -- groups <server>        # probe an AnyConnect gateway's group list + 2FA
cargo run -- connect <name> [otp]
cargo run -- disconnect <name>
```

## Usage

1. Tray icon → **Open VpncBar…** → **Add VPN…** (or **Import…** a `.pcf`/`.conf`).
2. Pick the **Type** — *Cisco IPSec (vpnc)* or *AnyConnect (openconnect)*; for
   openconnect use **Fetch groups** to fill the group list and auto-detect 2FA.
3. **Click a profile row** to connect; click again to disconnect. The row shows a
   ✓ and a live elapsed timer while up.
4. The **edit** (pencil) button opens the editor (Credentials / Options / Info /
   Debug + a Connect/Disconnect button). The trash button removes the profile.
5. Left-clicking the tray icon opens the window; the tray menu does quick
   connect/disconnect.

## Where things are stored

| What | Where |
|------|-------|
| Profiles (no secrets) | `~/.config/vpncbar/profiles.json` |
| Pidfiles + per-session logs | `~/.config/vpncbar/run/` |
| Live tunnel info (Info tab) | `/run/vpncbar/<uuid>.info` (written by the vpnc-script) |
| Secrets | Secret Service, items `vpnc-<uuid>-secret` / `…-password` |
| Installed files | `/usr/bin/vpncbar`, `/usr/lib/vpncbar/`, `/etc/polkit-1/rules.d/10-vpncbar.rules` |

## Security notes

- The passwordless polkit rule (`10-vpncbar.rules`) is scoped to exactly
  `vpnc`, `openconnect`, and the `vpncbar-disconnect` helper, and only for members
  of the `vpncbar` group. The disconnect helper verifies its target is really a
  vpnc/openconnect process before signalling it.
- Prefer prompting instead? Remove the rule and you'll get a polkit dialog per
  connect/disconnect; everything else still works.

## Licensing

GPLv2-or-later (the Rust app and the vendored shell scripts). `vpnc`/`openconnect`
and their `vpnc-script` are provided by your distribution.
