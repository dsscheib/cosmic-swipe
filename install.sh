#!/usr/bin/env bash
# Builds and installs cosmic-swipe for the current user.
# sudo is used for one thing only: installing the touchpad-only udev rule.
set -euo pipefail
cd "$(dirname "$0")"

RULE=/etc/udev/rules.d/70-cosmic-swipe-touchpad.rules

cargo build --release --locked
install -Dm755 target/release/cosmic-swipe "$HOME/.local/bin/cosmic-swipe"
install -Dm644 data/cosmic-swipe.service "$HOME/.config/systemd/user/cosmic-swipe.service"

if ! cmp -s data/70-cosmic-swipe-touchpad.rules "$RULE"; then
  echo "Installing the touchpad-only udev rule (needs sudo)..."
  sudo install -m644 data/70-cosmic-swipe-touchpad.rules "$RULE"
  sudo udevadm control --reload
  sudo udevadm trigger --subsystem-match=input --action=change
  udevadm settle
fi

systemctl --user daemon-reload
systemctl --user enable cosmic-swipe.service
systemctl --user restart cosmic-swipe.service
sleep 1
journalctl --user -u cosmic-swipe.service -n 5 --no-pager -o cat
echo
echo "Installed. Follow it live with: journalctl --user -u cosmic-swipe -f"
