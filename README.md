# cosmic-swipe

Four-finger up/down touchpad gestures for the COSMIC desktop, in Rust.

| Gesture | Overview closed | Overview open |
|---|---|---|
| 4 fingers up | Open the Workspaces overview | Nothing |
| 4 fingers down | Open the App Library | Close the overview |
| 4 fingers left/right | COSMIC's own workspace switching (untouched) | — |

COSMIC (cosmic-comp on Pop!_OS 24.04) handles four-finger left/right itself but ignores
up/down. This fills that gap until COSMIC ships it natively
([pop-os/cosmic-comp#2838](https://github.com/pop-os/cosmic-comp/pull/2838)); once it does,
uninstall this to avoid double actions.

## No `input` group needed

The touchpad is read directly through evdev. A udev rule
(`data/70-cosmic-swipe-touchpad.rules`) tags **touchpads only** with `uaccess`, so logind
gives the logged-in user an ACL on that one device. The keyboard and all other input devices
stay unreadable to your user, unlike joining the `input` group.

## Install

Requires COSMIC (tested on Pop!_OS 24.04 with COSMIC 1.11) and a Rust toolchain.

```sh
git clone https://github.com/dsscheib/cosmic-swipe
cd cosmic-swipe
./install.sh
```

Builds a release binary into `~/.local/bin`, installs the udev rule (the only step that uses
`sudo`) and enables a systemd user service. No logout needed.

```sh
journalctl --user -u cosmic-swipe -f     # see each swipe and the action taken
./uninstall.sh                           # remove everything, including the udev rule
```

## Tuning

`systemctl --user edit cosmic-swipe`, then:

```ini
[Service]
Environment=COSMIC_SWIPE_DISTANCE_MM=7    # travel before a swipe fires
Environment=COSMIC_SWIPE_DOMINANCE=1.5    # how clearly one axis must win over the other
Environment=COSMIC_SWIPE_DEVICE=/dev/input/event7   # pin a device instead of auto-detecting
```

`cosmic-swipe --dry-run` recognizes swipes and prints them without acting.

## How it works

| Piece | Mechanism |
|---|---|
| Touchpad | `evdev` crate: raw multitouch slots, palms excluded, finger count also from `BTN_TOOL_*` |
| Recognition | `src/gesture.rs`: average motion of the four fingers in mm (kernel resolution), one action per gesture, re-armed when all fingers lift. Unit tested. |
| Overview | `zbus`: `com.system76.CosmicWorkspaces` `Show` / `Hide`, open state from its `Shown` / `Hidden` signals |
| App Library | `cosmic-app-library`, the command Super+A runs |

Reading the touchpad is non-exclusive: COSMIC keeps receiving every event as usual.

## License

MIT
