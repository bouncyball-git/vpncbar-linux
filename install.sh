#!/bin/sh
# Build and install VpncBar (Linux). Run as your normal user; it sudo's only the
# privileged steps. Idempotent — safe to re-run to upgrade.
set -e
cd "$(dirname "$0")"

PREFIX=/usr
BIN="$PREFIX/bin/vpncbar"
LIBDIR="$PREFIX/lib/vpncbar"
# Desktop file + icon are named after the GApplication id so the Wayland
# compositor (KWin) resolves the window/taskbar icon from the window's app-id.
APP_ID=io.github.vpncbar
POLKIT=/etc/polkit-1/rules.d/10-vpncbar.rules
DESKTOP="$PREFIX/share/applications/$APP_ID.desktop"
ICON="$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"
GROUP=vpncbar

echo "==> Checking runtime dependencies"
missing=
command -v vpnc >/dev/null 2>&1        || missing="$missing vpnc"
command -v secret-tool >/dev/null 2>&1 || missing="$missing libsecret(secret-tool)"
command -v pkexec >/dev/null 2>&1      || missing="$missing polkit(pkexec)"
pkg-config --exists gtk4 2>/dev/null   || missing="$missing gtk4"
if [ -n "$missing" ]; then
    echo "   ! Missing:$missing"
    echo "     On Arch/Manjaro:  sudo pacman -S --needed vpnc openconnect libsecret polkit gtk4"
    exit 1
fi
command -v openconnect >/dev/null 2>&1 || \
    echo "   (note) openconnect not found — vpnc profiles work; install it for AnyConnect."

echo "==> Building release binary"
cargo build --release

echo "==> Installing (sudo)"
sudo install -Dm755 target/release/vpncbar       "$BIN"
sudo install -Dm755 packaging/vpncbar-script     "$LIBDIR/vpncbar-script"
sudo install -Dm755 packaging/vpncbar-disconnect "$LIBDIR/vpncbar-disconnect"
sudo install -Dm644 packaging/10-vpncbar.rules   "$POLKIT"
sudo install -Dm644 packaging/$APP_ID.desktop    "$DESKTOP"
sudo install -Dm644 packaging/lock.svg           "$ICON"   # lock = app icon
# Drop earlier vpncbar-named copies so the launcher doesn't show a stale duplicate.
sudo rm -f "$PREFIX/share/applications/vpncbar.desktop" \
           "$PREFIX/share/icons/hicolor/scalable/apps/vpncbar.svg"

echo "==> Refreshing desktop / icon caches"
sudo gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
sudo update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
# KDE caches the rendered menu icon; rebuild its caches and drop the stale one so
# the launcher shows the new icon without a re-login.
rm -f "$HOME/.cache/icon-cache.kcache" 2>/dev/null || true
kbuildsycoca6 >/dev/null 2>&1 || kbuildsycoca5 >/dev/null 2>&1 || true

echo "==> Setting up passwordless polkit group '$GROUP'"
getent group "$GROUP" >/dev/null || sudo groupadd -r "$GROUP"
if ! id -nG "$USER" | tr ' ' '\n' | grep -qx "$GROUP"; then
    sudo gpasswd -a "$USER" "$GROUP"
    echo "   Added $USER to '$GROUP' — log out/in (or run 'newgrp $GROUP') for it to take effect."
fi

echo
echo "Done. Launch from your menu (VpncBar) or run: vpncbar"
echo "To start it automatically on login:"
echo "    cp $DESKTOP ~/.config/autostart/"
