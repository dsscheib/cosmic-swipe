#!/usr/bin/env bash
# Removes cosmic-swipe and its udev rule.
set -euo pipefail
RULE=/etc/udev/rules.d/70-cosmic-swipe-touchpad.rules

systemctl --user disable --now cosmic-swipe.service 2>/dev/null || true
rm -f "$HOME/.local/bin/cosmic-swipe" "$HOME/.config/systemd/user/cosmic-swipe.service"
systemctl --user daemon-reload

if [ -e "$RULE" ]; then
  echo "Removing the udev rule (needs sudo)..."
  sudo rm -f "$RULE"
  sudo udevadm control --reload
  sudo udevadm trigger --subsystem-match=input --action=change
fi
echo "cosmic-swipe removed."
