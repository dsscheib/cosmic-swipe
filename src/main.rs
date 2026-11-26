//! cosmic-swipe: four-finger up/down touchpad gestures for the COSMIC desktop.
//!
//!   4 fingers up    open the Workspaces overview
//!   4 fingers down  close the overview, or open the App Library from the desktop
//!
//! COSMIC already handles four-finger left/right (workspace switching) itself, so those are
//! recognized only to keep a sideways swipe from turning into a vertical one, then ignored.
//!
//! The touchpad is read directly through evdev. It needs read access to the touchpad device
//! only, which the bundled udev rule grants to the logged-in user; the `input` group is not
//! needed. The overview is driven through its D-Bus API (com.system76.CosmicWorkspaces), and
//! its Shown/Hidden signals keep track of whether it is open.

mod gesture;
mod touchpad;

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use std::{env, thread};

use futures_util::StreamExt;
use zbus::{Connection, MatchRule, MessageStream, message};

use gesture::{Config, Detector, Direction, TapDetector};
use touchpad::Touchpad;

const WORKSPACES: &str = "com.system76.CosmicWorkspaces";
const WORKSPACES_PATH: &str = "/com/system76/CosmicWorkspaces";
const APP_LIBRARY_CMD: &str = "cosmic-app-library"; // what Super+A runs

const USAGE: &str = "\
Usage: cosmic-swipe [--device /dev/input/eventN] [--dry-run]

  --device PATH   touchpad to read (default: the first readable touchpad)
  --dry-run       recognize swipes and print them, but take no action

Environment:
  COSMIC_SWIPE_DISTANCE_MM  travel before a swipe fires (default 7)
  COSMIC_SWIPE_DOMINANCE    how clearly one axis must win (default 1.5)
  COSMIC_SWIPE_DEVICE       same as --device";

struct Args {
    device: Option<PathBuf>,
    dry_run: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        device: env::var_os("COSMIC_SWIPE_DEVICE").map(PathBuf::from),
        dry_run: false,
    };
    let mut it = env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--dry-run" => args.dry_run = true,
            "--device" => match it.next() {
                Some(p) => args.device = Some(PathBuf::from(p)),
                None => die("--device needs a path"),
            },
            "--version" => {
                println!("cosmic-swipe {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => die(&format!("unknown argument: {other}")),
        }
    }
    args
}

fn die(msg: &str) -> ! {
    eprintln!("cosmic-swipe: {msg}\n\n{USAGE}");
    std::process::exit(2);
}

fn env_f64(name: &str, default: f64) -> f64 {
    match env::var(name) {
        Ok(v) => v.trim().parse().unwrap_or_else(|_| {
            eprintln!("ignoring {name}={v}: not a number");
            default
        }),
        Err(_) => default,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    ShowOverview,
    HideOverview,
    OpenAppLibrary,
    CloseAppLibrary,
    None,
}

impl Action {
    /// What a swipe does, given what is open. The App Library takes precedence: opening the
    /// overview over it would close it anyway, as the overview takes keyboard focus.
    fn for_swipe(dir: Direction, overview_open: bool, library_open: bool) -> Self {
        match (dir, overview_open, library_open) {
            (Direction::Up, _, true) => Self::CloseAppLibrary,
            (Direction::Up, false, false) => Self::ShowOverview,
            (Direction::Down, true, _) => Self::HideOverview,
            (Direction::Down, false, false) => Self::OpenAppLibrary,
            // Down with the library already open, up with the overview already open, and
            // left/right, which COSMIC handles itself.
            _ => Self::None,
        }
    }
}

/// Calls into the Workspaces overview. Connects lazily and reconnects after an error, so the
/// daemon survives starting before the session bus is up or the overview restarting.
#[derive(Default)]
struct Overview {
    conn: Option<Connection>,
}

impl Overview {
    fn call(&mut self, method: &str) {
        let started = Instant::now();
        let result = zbus::block_on(async {
            if self.conn.is_none() {
                self.conn = Some(Connection::session().await?);
            }
            let conn = self.conn.as_ref().expect("connected above");
            conn.call_method(Some(WORKSPACES), WORKSPACES_PATH, Some(WORKSPACES), method, &())
                .await
                .map(drop)
        });
        match result {
            Ok(()) => eprintln!("  overview {method} took {} ms", started.elapsed().as_millis()),
            Err(e) => {
                eprintln!("overview {method} failed: {e}");
                self.conn = None;
            }
        }
    }
}

/// Keeps `shown` in step with the overview, from its Shown/Hidden signals. Runs forever on its
/// own thread with its own connection, reconnecting if the bus goes away.
fn watch_overview(shown: Arc<AtomicBool>, library_open: Arc<AtomicBool>) {
    loop {
        let result: zbus::Result<()> = zbus::block_on(async {
            let conn = Connection::session().await?;
            let signals = MatchRule::builder()
                .msg_type(message::Type::Signal)
                .interface(WORKSPACES)
                .path(WORKSPACES_PATH)
                .build()?;
            // If the overview process exits or restarts, it is no longer showing.
            let owner = MatchRule::builder()
                .msg_type(message::Type::Signal)
                .sender("org.freedesktop.DBus")
                .interface("org.freedesktop.DBus")
                .member("NameOwnerChanged")
                .arg(0, WORKSPACES)
                .build()?;
            let mut stream = futures_util::stream::select(
                MessageStream::for_match_rule(signals, &conn, None).await?,
                MessageStream::for_match_rule(owner, &conn, None).await?,
            );
            while let Some(msg) = stream.next().await {
                let msg = msg?;
                match msg.header().member().map(|m| m.as_str()) {
                    Some("Shown") => {
                        shown.store(true, Ordering::Relaxed);
                        // The overview takes keyboard focus, which closes the App Library.
                        library_open.store(false, Ordering::Relaxed);
                    }
                    Some("Hidden") | Some("NameOwnerChanged") => {
                        shown.store(false, Ordering::Relaxed)
                    }
                    _ => {}
                }
            }
            Ok(())
        });
        if let Err(e) = result {
            eprintln!("overview watcher: {e}; retrying");
        }
        shown.store(false, Ordering::Relaxed);
        thread::sleep(Duration::from_secs(2));
    }
}

fn spawn(cmd: &str) {
    // Detached: the child's exit status is not needed. `wait` happens on a short-lived thread
    // so the process never lingers as a zombie.
    match Command::new(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("could not run {cmd}: {e}"),
    }
}

fn main() {
    let args = parse_args();
    let distance_mm = env_f64("COSMIC_SWIPE_DISTANCE_MM", 7.0);
    let dominance = env_f64("COSMIC_SWIPE_DOMINANCE", 1.5);

    let shown = Arc::new(AtomicBool::new(false));
    // Whether the App Library is open, as far as this daemon can tell. It has no D-Bus state,
    // so this is tracked: set when a swipe opens it, cleared by anything that usually closes
    // it (a click or tap, the overview opening). Closing it from the keyboard (Esc, Super+A)
    // cannot be seen, since the keyboard is deliberately unreadable.
    let library_open = Arc::new(AtomicBool::new(false));
    if !args.dry_run {
        let (shown, library_open) = (shown.clone(), library_open.clone());
        thread::spawn(move || watch_overview(shown, library_open));
    }
    let mut overview = Overview::default();

    eprintln!(
        "cosmic-swipe {} started{}",
        env!("CARGO_PKG_VERSION"),
        if args.dry_run { " (dry run)" } else { "" }
    );

    // Reopen the touchpad whenever it goes away (suspend, unplug) or is not there yet.
    let mut last_error = String::new();
    loop {
        match Touchpad::open(args.device.as_deref()) {
            Ok(mut tp) => {
                last_error.clear();
                eprintln!(
                    "reading {} ({}), {:.0}x{:.0} units/mm",
                    tp.path.display(),
                    tp.name,
                    tp.units_per_mm_x,
                    tp.units_per_mm_y
                );
                let mut det = Detector::new(Config {
                    distance_mm,
                    dominance,
                    units_per_mm_x: tp.units_per_mm_x,
                    units_per_mm_y: tp.units_per_mm_y,
                });
                let mut taps = TapDetector::new(tp.units_per_mm_x);
                let mut gesture_start = 0.0;
                let mut prev_fingers = 0;
                let err = tp.run(|f| {
                    if f.fingers == 4 && prev_fingers != 4 {
                        gesture_start = f.time;
                    }
                    prev_fingers = f.fingers;

                    let tapped = taps.frame(f.time, f.touches);
                    if (f.clicked || tapped) && library_open.swap(false, Ordering::Relaxed) {
                        eprintln!("{} -> assuming the App Library closed",
                            if f.clicked { "click" } else { "tap" });
                    }

                    let Some(dir) = det.frame(f.fingers, f.touches) else { return };
                    let action = Action::for_swipe(
                        dir,
                        shown.load(Ordering::Relaxed),
                        library_open.load(Ordering::Relaxed),
                    );
                    eprintln!(
                        "swipe {dir:?} after {:.0} ms -> {action:?}",
                        (f.time - gesture_start) * 1000.0
                    );
                    if args.dry_run {
                        return;
                    }
                    match action {
                        Action::ShowOverview => overview.call("Show"),
                        Action::HideOverview => overview.call("Hide"),
                        Action::OpenAppLibrary => {
                            spawn(APP_LIBRARY_CMD);
                            library_open.store(true, Ordering::Relaxed);
                        }
                        Action::CloseAppLibrary => {
                            spawn(APP_LIBRARY_CMD); // the command toggles it
                            library_open.store(false, Ordering::Relaxed);
                        }
                        Action::None => {}
                    }
                });
                if let Err(e) = err {
                    eprintln!("touchpad lost: {e}; reopening");
                }
            }
            Err(e) => {
                // Log once per distinct error instead of every retry.
                let msg = e.to_string();
                if msg != last_error {
                    eprintln!("cannot open touchpad: {msg}; retrying every 2 s");
                    last_error = msg;
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Direction::*;

    #[test]
    fn up_from_app_library_closes_it_instead_of_opening_the_overview() {
        assert_eq!(Action::for_swipe(Up, false, true), Action::CloseAppLibrary);
    }

    #[test]
    fn swipe_table() {
        // (direction, overview open, library open) -> action
        let cases = [
            (Up, false, false, Action::ShowOverview),
            (Up, true, false, Action::None),
            (Down, false, false, Action::OpenAppLibrary),
            (Down, false, true, Action::None),
            (Down, true, false, Action::HideOverview),
            (Left, false, false, Action::None),
            (Right, true, false, Action::None),
        ];
        for (dir, ov, lib, want) in cases {
            assert_eq!(Action::for_swipe(dir, ov, lib), want, "{dir:?} ov={ov} lib={lib}");
        }
    }
}
