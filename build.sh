#!/bin/sh
# Build VpncBar. Usage: ./build.sh [release|debug|clean]   (default: release)
# Installation is separate: run ./install.sh [release|debug] afterwards.
set -e
cd "$(dirname "$0")"

PROFILE="${1:-release}"
case "$PROFILE" in
    release | debug) ;;
    clean)
        echo "==> Cleaning build artifacts"
        cargo clean
        exit 0
        ;;
    *) echo "usage: $0 [release|debug|clean]" >&2; exit 1 ;;
esac

# gtk4 headers are a build dependency (runtime checks live in install.sh).
if ! pkg-config --exists gtk4 2>/dev/null; then
    echo "gtk4 development files not found."
    echo "On Arch/Manjaro:  sudo pacman -S --needed gtk4"
    exit 1
fi

echo "==> Building $PROFILE binary"
if [ "$PROFILE" = release ]; then
    cargo build --release
else
    cargo build
fi

echo "Built target/$PROFILE/vpncbar"
