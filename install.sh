#!/bin/sh
# Build and install VpncBar (Linux). Run as your normal user; it sudo's only the
# privileged steps. Idempotent — safe to re-run to upgrade.
set -e
cd "$(dirname "$0")"

PREFIX=/usr
BIN="$PREFIX/bin/vpncbar"
LIBDIR="$PREFIX/lib/vpncbar"
POLKIT=/etc/polkit-1/rules.d/10-vpncbar.rules
DESKTOP="$PREFIX/share/applications/vpncbar.desktop"
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
sudo install -Dm644 packaging/vpncbar.desktop    "$DESKTOP"

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
