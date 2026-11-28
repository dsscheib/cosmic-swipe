//! Switching workspaces through the Wayland ext-workspace-v1 protocol, which COSMIC offers to
//! ordinary clients.
//!
//! A background thread keeps a Wayland connection open and a live list of workspace groups
//! (one per monitor in COSMIC's default per-monitor mode) and their workspaces. A switch is sent
//! from the caller's thread: `activate` on the neighbouring workspace, then `commit`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use std::{env, fs, thread};

use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, event_created_child};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1::{self, ExtWorkspaceGroupHandleV1},
    ext_workspace_handle_v1::{self, ExtWorkspaceHandleV1},
    ext_workspace_manager_v1::{self, ExtWorkspaceManagerV1},
};

struct Workspace {
    handle: ExtWorkspaceHandleV1,
    name: String,
    coordinates: Vec<u32>,
    active: bool,
    hidden: bool,
}

struct Group {
    handle: ExtWorkspaceGroupHandleV1,
    /// Member workspaces, in the order the compositor announced them.
    members: Vec<ObjectId>,
}

#[derive(Default)]
struct Live {
    conn: Option<Connection>,
    manager: Option<ExtWorkspaceManagerV1>,
    groups: Vec<Group>,
    workspaces: HashMap<ObjectId, Workspace>,
}

/// Handle for switching workspaces. Cheap to share; the connection lives on its own thread.
#[derive(Clone, Default)]
pub struct Workspaces {
    live: Arc<Mutex<Live>>,
}

impl Workspaces {
    /// Starts the background thread that connects (and reconnects) to the compositor.
    pub fn start() -> Self {
        let ws = Self::default();
        let live = ws.live.clone();
        thread::spawn(move || watch(live));
        ws
    }

    fn lock(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Moves `step` workspaces along (+1 next, -1 previous) and returns the new workspace's
    /// name.
    pub fn switch(&self, step: i32) -> Result<String, String> {
        let live = self.lock();
        let (Some(conn), Some(manager)) = (&live.conn, &live.manager) else {
            return Err("not connected to the compositor".into());
        };
        // COSMIC gives each monitor its own group. With several, the first is used: the
        // protocol does not say which monitor the overview or the pointer is on.
        let group = live
            .groups
            .iter()
            .find(|g| !g.members.is_empty())
            .ok_or("no workspaces reported")?;
        if live.groups.len() > 1 {
            eprintln!("  several monitors: switching on the first one's workspaces");
        }

        let mut order: Vec<&Workspace> = group
            .members
            .iter()
            .filter_map(|id| live.workspaces.get(id))
            .filter(|w| !w.hidden)
            .collect();
        // Stable: workspaces without coordinates keep the announced order.
        order.sort_by(|a, b| a.coordinates.cmp(&b.coordinates));
        let active: Vec<bool> = order.iter().map(|w| w.active).collect();

        let target = neighbour(&active, step, wraparound()).ok_or("already at the end")?;
        order[target].handle.activate();
        manager.commit();
        conn.flush().map_err(|e| e.to_string())?;
        Ok(order[target].name.clone())
    }
}

/// Index of the workspace `step` places from the active one, or None if that runs off the end
/// without wraparound (or nothing is active).
fn neighbour(active: &[bool], step: i32, wrap: bool) -> Option<usize> {
    let n = i64::try_from(active.len()).ok()?;
    let cur = i64::try_from(active.iter().position(|&a| a)?).ok()?;
    let to = cur + i64::from(step);
    let to = if wrap { to.rem_euclid(n) } else { to };
    (0..n).contains(&to).then_some(to as usize).filter(|&t| t as i64 != cur)
}

/// COSMIC's "workspace_wraparound" setting, read at each switch so changes apply at once.
fn wraparound() -> bool {
    let Some(home) = env::var_os("HOME") else { return false };
    let path = std::path::Path::new(&home)
        .join(".config/cosmic/com.system76.CosmicComp/v1/workspaces");
    fs::read_to_string(path)
        .map(|s| s.contains("workspace_wraparound: true"))
        .unwrap_or(false)
}

/// Keeps a connection open and `live` up to date, forever, reconnecting after errors.
fn watch(live: Arc<Mutex<Live>>) {
    let mut last_error = String::new();
    loop {
        if let Err(e) = run(&live) {
            let msg = e.to_string();
            if msg != last_error {
                eprintln!("workspaces: {msg}; retrying every 2 s");
                last_error = msg;
            }
        }
        *live.lock().unwrap_or_else(|e| e.into_inner()) = Live::default();
        thread::sleep(Duration::from_secs(2));
    }
}

fn run(live: &Arc<Mutex<Live>>) -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::connect_to_env()?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let manager: ExtWorkspaceManagerV1 = globals.bind(&qh, 1..=1, ())?;
    {
        let mut l = live.lock().unwrap_or_else(|e| e.into_inner());
        l.conn = Some(conn);
        l.manager = Some(manager);
    }
    let mut state = State { live: live.clone() };
    queue.roundtrip(&mut state)?;
    {
        let l = state.lock();
        let active: Vec<&str> =
            l.workspaces.values().filter(|w| w.active).map(|w| w.name.as_str()).collect();
        eprintln!(
            "workspaces: connected, {} monitor group(s), {} workspaces, active {active:?}",
            l.groups.len(),
            l.workspaces.len()
        );
    }
    loop {
        queue.blocking_dispatch(&mut state)?;
    }
}

struct State {
    live: Arc<Mutex<Live>>,
}

impl State {
    fn lock(&self) -> MutexGuard<'_, Live> {
        self.live.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtWorkspaceManagerV1,
        event: ext_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut live = state.lock();
        match event {
            ext_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } => {
                live.groups.push(Group { handle: workspace_group, members: Vec::new() });
            }
            ext_workspace_manager_v1::Event::Workspace { workspace } => {
                live.workspaces.insert(
                    workspace.id(),
                    Workspace {
                        handle: workspace,
                        name: String::new(),
                        coordinates: Vec::new(),
                        active: false,
                        hidden: false,
                    },
                );
            }
            ext_workspace_manager_v1::Event::Finished => live.manager = None,
            _ => {}
        }
    }

    event_created_child!(State, ExtWorkspaceManagerV1, [
        ext_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (ExtWorkspaceGroupHandleV1, ()),
        ext_workspace_manager_v1::EVT_WORKSPACE_OPCODE => (ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        group: &ExtWorkspaceGroupHandleV1,
        event: ext_workspace_group_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut live = state.lock();
        match event {
            ext_workspace_group_handle_v1::Event::WorkspaceEnter { workspace } => {
                if let Some(g) = live.groups.iter_mut().find(|g| g.handle == *group) {
                    g.members.push(workspace.id());
                }
            }
            ext_workspace_group_handle_v1::Event::WorkspaceLeave { workspace } => {
                if let Some(g) = live.groups.iter_mut().find(|g| g.handle == *group) {
                    g.members.retain(|id| *id != workspace.id());
                }
            }
            ext_workspace_group_handle_v1::Event::Removed => {
                live.groups.retain(|g| g.handle != *group);
                group.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ExtWorkspaceHandleV1,
        event: ext_workspace_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let mut live = state.lock();
        if let ext_workspace_handle_v1::Event::Removed = event {
            live.workspaces.remove(&handle.id());
            for g in &mut live.groups {
                g.members.retain(|id| *id != handle.id());
            }
            handle.destroy();
            return;
        }
        let Some(w) = live.workspaces.get_mut(&handle.id()) else { return };
        match event {
            ext_workspace_handle_v1::Event::Name { name } => w.name = name,
            ext_workspace_handle_v1::Event::Coordinates { coordinates } => {
                let (chunks, _) = coordinates.as_chunks::<4>();
                w.coordinates = chunks.iter().map(|c| u32::from_ne_bytes(*c)).collect();
            }
            ext_workspace_handle_v1::Event::State { state } => {
                let bits = match state {
                    WEnum::Value(s) => s.bits(),
                    WEnum::Unknown(b) => b,
                };
                w.active = bits & ext_workspace_handle_v1::State::Active.bits() != 0;
                w.hidden = bits & ext_workspace_handle_v1::State::Hidden.bits() != 0;
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::neighbour;

    #[test]
    fn steps_to_the_neighbour() {
        let ws = [false, true, false];
        assert_eq!(neighbour(&ws, 1, false), Some(2));
        assert_eq!(neighbour(&ws, -1, false), Some(0));
    }

    #[test]
    fn stops_at_the_ends_without_wraparound() {
        assert_eq!(neighbour(&[true, false], -1, false), None);
        assert_eq!(neighbour(&[false, true], 1, false), None);
    }

    #[test]
    fn wraps_around_when_enabled() {
        assert_eq!(neighbour(&[true, false, false], -1, true), Some(2));
        assert_eq!(neighbour(&[false, false, true], 1, true), Some(0));
    }

    #[test]
    fn single_or_no_active_workspace_goes_nowhere() {
        assert_eq!(neighbour(&[true], 1, true), None);
        assert_eq!(neighbour(&[false, false], 1, true), None);
        assert_eq!(neighbour(&[], 1, true), None);
    }
}
