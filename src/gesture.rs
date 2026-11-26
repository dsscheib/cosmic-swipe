//! Four-finger swipe recognition from raw multitouch frames.
//!
//! This module is pure logic: it takes one frame of touch points at a time and decides when a
//! four-finger swipe has travelled far enough, and in which direction. It knows nothing about
//! devices or D-Bus, so it can be unit tested without hardware.

/// Direction of a recognized swipe, in screen terms (up = fingers moved toward the top).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// One finger on the touchpad in a single frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Touch {
    /// Multitouch slot, stable for as long as the finger stays down.
    pub slot: i32,
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Travel along the main axis, in millimetres, before a swipe fires.
    pub distance_mm: f64,
    /// The main axis must beat the other one by this factor.
    pub dominance: f64,
    /// Touchpad units per millimetre on each axis (from the kernel's absinfo resolution).
    pub units_per_mm_x: f64,
    pub units_per_mm_y: f64,
}

/// Fires at most one direction per four-finger gesture.
///
/// A gesture starts when exactly four fingers are down and ends when the count changes.
/// Motion is the average movement of the fingers present in both consecutive frames, so a
/// finger landing or lifting never shows up as a jump.
pub struct Detector {
    cfg: Config,
    prev: Vec<Touch>,
    /// Accumulated travel of the current four-finger gesture, in millimetres.
    dx: f64,
    dy: f64,
    /// Whether the current gesture already fired.
    fired: bool,
}

impl Detector {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            prev: Vec::new(),
            dx: 0.0,
            dy: 0.0,
            fired: false,
        }
    }

    /// Feeds one frame (every finger currently down, palms already excluded) and returns a
    /// direction the first time the current four-finger gesture crosses the threshold.
    ///
    /// `fingers` is the finger count. It is usually `touches.len()`, but touchpads with fewer
    /// multitouch slots than fingers report the true count through BTN_TOOL_QUADTAP, so the
    /// caller passes the larger of the two.
    pub fn frame(&mut self, fingers: usize, touches: &[Touch]) -> Option<Direction> {
        let prev_fingers = self.prev.len();
        let prev = std::mem::replace(&mut self.prev, touches.to_vec());

        if fingers != 4 {
            self.dx = 0.0;
            self.dy = 0.0;
            if fingers == 0 {
                // Re-arm only once every finger is up, so a finger briefly dropping out
                // in the middle of a long swipe cannot fire it a second time.
                self.fired = false;
            }
            return None;
        }
        if prev_fingers != touches.len() {
            // The set of fingers changed: start measuring from this frame.
            self.dx = 0.0;
            self.dy = 0.0;
            return None;
        }
        if self.fired {
            return None;
        }

        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for t in touches {
            if let Some(p) = prev.iter().find(|p| p.slot == t.slot) {
                sx += f64::from(t.x - p.x);
                sy += f64::from(t.y - p.y);
                n += 1.0;
            }
        }
        if n == 0.0 {
            return None;
        }
        self.dx += sx / n / self.cfg.units_per_mm_x;
        self.dy += sy / n / self.cfg.units_per_mm_y;

        let (ax, ay) = (self.dx.abs(), self.dy.abs());
        let dir = if ay >= self.cfg.distance_mm && ay > self.cfg.dominance * ax {
            Some(if self.dy < 0.0 { Direction::Up } else { Direction::Down })
        } else if ax >= self.cfg.distance_mm && ax > self.cfg.dominance * ay {
            // Reported so a sideways swipe uses up the gesture and cannot turn into a
            // vertical one halfway through. COSMIC handles left/right itself.
            Some(if self.dx < 0.0 { Direction::Left } else { Direction::Right })
        } else {
            None
        };
        if dir.is_some() {
            self.fired = true;
        }
        dir
    }
}

/// Recognizes a tap: fingers down briefly and lifted without moving. Taps (and physical clicks,
/// which the caller sees directly) are how the App Library usually gets closed, by launching an
/// app or clicking away, so they are used to forget that it is open.
pub struct TapDetector {
    /// Start time and first positions of the current touch sequence.
    start: Option<(f64, Vec<Touch>)>,
    moved_mm: f64,
    units_per_mm: f64,
}

impl TapDetector {
    const MAX_SECS: f64 = 0.25;
    const MAX_MOVE_MM: f64 = 2.0;

    pub fn new(units_per_mm: f64) -> Self {
        Self { start: None, moved_mm: 0.0, units_per_mm }
    }

    /// Feeds one frame; returns true when the fingers lift and the sequence was a tap.
    pub fn frame(&mut self, time: f64, touches: &[Touch]) -> bool {
        match (&self.start, touches.is_empty()) {
            (None, false) => {
                self.start = Some((time, touches.to_vec()));
                self.moved_mm = 0.0;
                false
            }
            (Some((_, first)), false) => {
                for t in touches {
                    if let Some(f) = first.iter().find(|f| f.slot == t.slot) {
                        let d = f64::from(t.x - f.x).hypot(f64::from(t.y - f.y));
                        self.moved_mm = self.moved_mm.max(d / self.units_per_mm);
                    }
                }
                false
            }
            (Some((t0, _)), true) => {
                let tap = time - t0 <= Self::MAX_SECS && self.moved_mm <= Self::MAX_MOVE_MM;
                self.start = None;
                tap
            }
            (None, true) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CFG: Config = Config {
        distance_mm: 12.0,
        dominance: 1.5,
        units_per_mm_x: 10.0,
        units_per_mm_y: 10.0,
    };

    /// Four fingers side by side, offset by (x, y) touchpad units.
    fn four(x: i32, y: i32) -> Vec<Touch> {
        (0..4)
            .map(|i| Touch { slot: i, x: 300 + i * 80 + x, y: 400 + y })
            .collect()
    }

    /// Runs a straight swipe of `steps` frames moving (sx, sy) units per frame.
    fn swipe(det: &mut Detector, sx: i32, sy: i32, steps: i32) -> Vec<Direction> {
        (0..=steps)
            .filter_map(|i| det.frame(4, &four(i * sx, i * sy)))
            .collect()
    }

    #[test]
    fn swipe_up_fires_once() {
        let mut det = Detector::new(CFG);
        // 30 frames × 10 units = 300 units = 30 mm upward
        assert_eq!(swipe(&mut det, 0, -10, 30), vec![Direction::Up]);
    }

    #[test]
    fn swipe_down() {
        let mut det = Detector::new(CFG);
        assert_eq!(swipe(&mut det, 1, 10, 30), vec![Direction::Down]);
    }

    #[test]
    fn sideways_swipe_never_turns_vertical() {
        let mut det = Detector::new(CFG);
        let mut fired = swipe(&mut det, -10, 0, 20);
        // keep going upward within the same gesture
        fired.extend((1..=40).filter_map(|i| det.frame(4, &four(-200, -10 * i))));
        assert_eq!(fired, vec![Direction::Left]);
    }

    #[test]
    fn short_swipe_does_nothing() {
        let mut det = Detector::new(CFG);
        assert!(swipe(&mut det, 0, -10, 10).is_empty()); // 10 mm < 12 mm
    }

    #[test]
    fn diagonal_swipe_does_nothing() {
        let mut det = Detector::new(CFG);
        assert!(swipe(&mut det, 10, -10, 30).is_empty());
    }

    #[test]
    fn three_or_five_fingers_are_ignored() {
        let mut det = Detector::new(CFG);
        for i in 0..40 {
            let t = four(0, -10 * i);
            assert_eq!(det.frame(3, &t[..3]), None);
        }
        for i in 0..40 {
            let mut t = four(0, -10 * i);
            t.push(Touch { slot: 4, x: 900, y: 400 - 10 * i });
            assert_eq!(det.frame(5, &t), None);
        }
    }

    #[test]
    fn lifting_rearms_for_the_next_swipe() {
        let mut det = Detector::new(CFG);
        assert_eq!(swipe(&mut det, 0, -10, 30), vec![Direction::Up]);
        assert_eq!(det.frame(0, &[]), None);
        assert_eq!(swipe(&mut det, 0, 10, 30), vec![Direction::Down]);
    }

    #[test]
    fn finger_dropout_mid_swipe_does_not_fire_twice() {
        let mut det = Detector::new(CFG);
        assert_eq!(swipe(&mut det, 0, -10, 30), vec![Direction::Up]);
        // one finger briefly lost, then the long swipe continues
        assert_eq!(det.frame(3, &four(0, -310)[..3]), None);
        let more: Vec<_> = (32..=80)
            .filter_map(|i| det.frame(4, &four(0, -10 * i)))
            .collect();
        assert!(more.is_empty());
    }

    #[test]
    fn tap_is_recognized() {
        let mut tap = TapDetector::new(10.0);
        let one = |y| [Touch { slot: 0, x: 500, y }];
        assert!(!tap.frame(0.00, &one(400)));
        assert!(!tap.frame(0.05, &one(405)));
        assert!(tap.frame(0.10, &[]));
    }

    #[test]
    fn slow_or_moving_touch_is_not_a_tap() {
        let mut tap = TapDetector::new(10.0);
        let one = |y| [Touch { slot: 0, x: 500, y }];
        // held too long
        tap.frame(0.0, &one(400));
        assert!(!tap.frame(0.5, &[]));
        // moved 5 mm (pointer motion)
        tap.frame(1.0, &one(400));
        tap.frame(1.05, &one(450));
        assert!(!tap.frame(1.1, &[]));
    }

    #[test]
    fn finger_landing_is_not_a_jump() {
        let mut det = Detector::new(CFG);
        // three fingers down, then a fourth lands far away: no travel counted
        assert_eq!(det.frame(3, &four(0, 0)[..3]), None);
        let mut t = four(0, 0);
        t[3].y -= 500;
        assert_eq!(det.frame(4, &t), None);
        assert_eq!(det.frame(4, &t), None);
    }

    #[test]
    fn quadtap_count_with_fewer_slots() {
        // A touchpad with only two slots still reports four fingers via BTN_TOOL_QUADTAP.
        let mut det = Detector::new(CFG);
        let fired: Vec<_> = (0..=30)
            .filter_map(|i| det.frame(4, &four(0, -10 * i)[..2]))
            .collect();
        assert_eq!(fired, vec![Direction::Up]);
    }
}
