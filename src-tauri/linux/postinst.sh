#!/bin/sh
# Reload udev so the Rockchip USB rule applies without a reboot.
set -e
if command -v udevadm >/dev/null 2>&1; then
  udevadm control --reload-rules || true
  udevadm trigger --subsystem-match=usb || true
fi
