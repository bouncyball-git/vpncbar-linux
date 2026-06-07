#!/bin/sh
# Remove VpncBar (Linux). Keeps your profiles (~/.config/vpncbar) and Secret
# Service entries. Run as your normal user; it sudo's the privileged steps.
set -e

PREFIX=/usr

# Undo what vpncbar-setup changed (group memberships it added, DNS settings),
# from its recorded state — must run before the script itself is removed.
if [ -x "$PREFIX/bin/vpncbar-setup" ]; then
    sudo "$PREFIX/bin/vpncbar-setup" restore || true
fi

echo "==> Removing installed files (sudo)"
sudo rm -f \
    "$PREFIX/bin/vpncbar" \
    "$PREFIX/bin/vpncbar-setup" \
    "$PREFIX/lib/vpncbar/vpncbar-script" \
    "$PREFIX/lib/vpncbar/vpncbar-disconnect" \
    /etc/polkit-1/rules.d/10-vpncbar.rules \
    "$PREFIX/share/applications/io.github.vpncbar.desktop" \
    "$PREFIX/share/applications/vpncbar.desktop" \
    "$PREFIX/share/icons/hicolor/scalable/apps/io.github.vpncbar.svg" \
    "$PREFIX/share/icons/hicolor/scalable/apps/vpncbar.svg"
sudo rmdir "$PREFIX/lib/vpncbar" 2>/dev/null || true
rm -f ~/.config/autostart/io.github.vpncbar.desktop ~/.config/autostart/vpncbar.desktop 2>/dev/null || true
sudo gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true
kbuildsycoca6 >/dev/null 2>&1 || kbuildsycoca5 >/dev/null 2>&1 || true

# (group memberships, the group itself if empty, and DNS settings were
#  restored above by 'vpncbar-setup restore')
echo
echo "Your profiles (~/.config/vpncbar) and stored secrets are kept."
echo "For a full wipe:  rm -rf ~/.config/vpncbar  and clear 'vpnc-*' items from your keyring."
