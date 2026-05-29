#!/bin/sh
# Remove VpncBar (Linux). Keeps your profiles (~/.config/vpncbar) and Secret
# Service entries. Run as your normal user; it sudo's the privileged steps.
set -e

PREFIX=/usr
echo "==> Removing installed files (sudo)"
sudo rm -f \
    "$PREFIX/bin/vpncbar" \
    "$PREFIX/lib/vpncbar/vpncbar-script" \
    "$PREFIX/lib/vpncbar/vpncbar-disconnect" \
    /etc/polkit-1/rules.d/10-vpncbar.rules \
    "$PREFIX/share/applications/vpncbar.desktop"
sudo rmdir "$PREFIX/lib/vpncbar" 2>/dev/null || true
rm -f ~/.config/autostart/vpncbar.desktop 2>/dev/null || true

echo "==> Leaving the 'vpncbar' group in place (remove manually if desired:"
echo "    sudo gpasswd -d \$USER vpncbar ; sudo groupdel vpncbar )"
echo
echo "Your profiles (~/.config/vpncbar) and stored secrets are kept."
echo "For a full wipe:  rm -rf ~/.config/vpncbar  and clear 'vpnc-*' items from your keyring."
