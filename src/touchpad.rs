//! Finding the touchpad and turning its raw evdev stream into multitouch frames.
//!
//! Only the touchpad is ever opened. With the touchpad-only udev rule, every other input
//! device (keyboard included) stays unreadable to this user, and `evdev::enumerate()` skips
//! devices it cannot open.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use evdev::{AbsoluteAxisCode as Abs, Device, EventSummary, KeyCode, PropType, SynchronizationCode};

use crate::gesture::Touch;

/// Everything the touchpad reported up to one SYN_REPORT.
pub struct Frame<'a> {
    /// Kernel event time, in seconds since the Unix epoch.
    pub time: f64,
    /// Finger count, palms excluded (the larger of tracked slots and BTN_TOOL_*).
    pub fingers: usize,
    /// Tracked fingers with known positions, palms excluded.
    pub touches: &'a [Touch],
    /// The physical button (clickpad press) went down in this frame.
    pub clicked: bool,
}

/// ABS_MT_TOOL_TYPE value the kernel uses for a palm.
const MT_TOOL_PALM: i32 = 2;
/// Used when the kernel reports no resolution, which some touchpads do.
const FALLBACK_UNITS_PER_MM: f64 = 30.0;

pub struct Touchpad {
    pub path: PathBuf,
    pub name: String,
    pub units_per_mm_x: f64,
    pub units_per_mm_y: f64,
    dev: Device,
}

fn is_touchpad(dev: &Device) -> bool {
    // Touchpads are indirect pointing devices with multitouch positions and finger-count keys.
    // Touchscreens set INPUT_PROP_DIRECT instead of INPUT_PROP_POINTER.
    dev.properties().contains(PropType::POINTER)
        && !dev.properties().contains(PropType::DIRECT)
        && dev
            .supported_absolute_axes()
            .is_some_and(|a| a.contains(Abs::ABS_MT_POSITION_X) && a.contains(Abs::ABS_MT_POSITION_Y))
        && dev
            .supported_keys()
            .is_some_and(|k| k.contains(KeyCode::BTN_TOOL_FINGER))
}

impl Touchpad {
    /// Opens `path` if given, otherwise the first touchpad this user can read.
    pub fn open(path: Option<&Path>) -> io::Result<Self> {
        let (path, dev) = match path {
            Some(p) => (p.to_path_buf(), Device::open(p)?),
            None => evdev::enumerate().find(|(_, d)| is_touchpad(d)).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "no readable touchpad (is the udev rule installed?)",
                )
            })?,
        };

        let mut res_x = 0;
        let mut res_y = 0;
        for (axis, info) in dev.get_absinfo()? {
            if axis == Abs::ABS_MT_POSITION_X {
                res_x = info.resolution();
            } else if axis == Abs::ABS_MT_POSITION_Y {
                res_y = info.resolution();
            }
        }
        let per_mm = |r: i32| if r > 0 { f64::from(r) } else { FALLBACK_UNITS_PER_MM };

        Ok(Self {
            path,
            name: dev.name().unwrap_or("unknown").to_owned(),
            units_per_mm_x: per_mm(res_x),
            units_per_mm_y: per_mm(res_y),
            dev,
        })
    }

    /// Reads events forever, calling `on_frame` at every SYN_REPORT.
    /// Returns only on a read error, such as the device going away on suspend or unplug.
    pub fn run(&mut self, mut on_frame: impl FnMut(&Frame)) -> io::Result<()> {
        #[derive(Default)]
        struct Slot {
            x: Option<i32>,
            y: Option<i32>,
            palm: bool,
        }

        let mut slots: BTreeMap<i32, Slot> = BTreeMap::new();
        let mut cur: i32 = 0;
        // Finger count from BTN_TOOL_*: covers touchpads with fewer slots than fingers.
        let mut tool_fingers: usize = 0;
        let mut frame: Vec<Touch> = Vec::with_capacity(5);
        let mut clicked = false;

        loop {
            for ev in self.dev.fetch_events()? {
                match ev.destructure() {
                    EventSummary::AbsoluteAxis(_, Abs::ABS_MT_SLOT, v) => cur = v,
                    EventSummary::AbsoluteAxis(_, Abs::ABS_MT_TRACKING_ID, v) => {
                        if v < 0 {
                            slots.remove(&cur);
                        } else {
                            slots.insert(cur, Slot::default());
                        }
                    }
                    EventSummary::AbsoluteAxis(_, axis, v) => {
                        if let Some(s) = slots.get_mut(&cur) {
                            match axis {
                                Abs::ABS_MT_POSITION_X => s.x = Some(v),
                                Abs::ABS_MT_POSITION_Y => s.y = Some(v),
                                Abs::ABS_MT_TOOL_TYPE => s.palm = v == MT_TOOL_PALM,
                                _ => {}
                            }
                        }
                    }
                    EventSummary::Key(_, KeyCode::BTN_LEFT, v) => clicked |= v == 1,
                    EventSummary::Key(_, key, v) => {
                        let n = match key {
                            KeyCode::BTN_TOOL_FINGER => 1,
                            KeyCode::BTN_TOOL_DOUBLETAP => 2,
                            KeyCode::BTN_TOOL_TRIPLETAP => 3,
                            KeyCode::BTN_TOOL_QUADTAP => 4,
                            KeyCode::BTN_TOOL_QUINTTAP => 5,
                            _ => continue,
                        };
                        if v != 0 {
                            tool_fingers = n;
                        } else if tool_fingers == n {
                            tool_fingers = 0;
                        }
                    }
                    EventSummary::Synchronization(sync, SynchronizationCode::SYN_REPORT, _) => {
                        frame.clear();
                        frame.extend(slots.iter().filter(|(_, s)| !s.palm).filter_map(|(&slot, s)| {
                            Some(Touch { slot, x: s.x?, y: s.y? })
                        }));
                        let palms = slots.values().filter(|s| s.palm).count();
                        let fingers = frame.len().max(tool_fingers.saturating_sub(palms));
                        let time = sync
                            .timestamp()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0.0, |d| d.as_secs_f64());
                        on_frame(&Frame { time, fingers, touches: &frame, clicked });
                        clicked = false;
                    }
                    _ => {}
                }
            }
        }
    }
}
