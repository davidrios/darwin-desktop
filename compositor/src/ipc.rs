//! A subset of Hyprland's IPC, for waybar's `hyprland/*` modules and
//! `hyprctl`-style scripts.
//!
//! Two unix sockets in `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/`:
//! - `.socket.sock`: one request per connection (`[flags]/command args`),
//!   answered and closed. `j/` asks for JSON (everything here answers JSON).
//!   Queries: `monitors`, `workspaces`, `activeworkspace`, `clients`,
//!   `activewindow`, `workspacerules`, `devices`, `version`, `systeminfo`.
//!   Dispatch: Lua (`/dispatch hl.dsp.focus({ workspace = "2" })`, what
//!   waybar sends once `systeminfo` says `configProvider: lua`), evaluated by
//!   the config's own `hl.dispatch`; or the legacy `dispatch workspace 2`
//!   form for the common dispatchers.
//! - `.socket2.sock`: events, `name>>data` lines. They come from comparing a
//!   snapshot of workspaces, windows, focus and keyboard layout each frame
//!   with the previous one.
//!
//! The signature is short (`dd_<pid>`): macOS caps socket paths at 103 bytes
//! and `$TMPDIR` is long. Window addresses are per-window counters.

use std::{
    collections::BTreeMap,
    io::{Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use smithay::{
    desktop::Window,
    reexports::{
        calloop::{
            Interest, LoopHandle, Mode, PostAction,
            generic::Generic,
        },
        wayland_server::Resource,
    },
    wayland::{compositor::with_states, seat::WaylandFocus, shell::xdg::XdgToplevelSurfaceData},
};

use crate::{
    binds::{Action, WorkspaceTarget},
    state::State,
};

/// The output name, as `wl_output` reports it (waybar matches monitors by it).
const MONITOR: &str = "darwin-0";
/// The Hyprland version whose IPC this follows.
const VERSION: &str = "0.57.2";

pub struct Ipc {
    dir: Option<PathBuf>,
    subscribers: Vec<UnixStream>,
    last: Snapshot,
}

impl Drop for Ipc {
    fn drop(&mut self) {
        if let Some(dir) = &self.dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

impl Ipc {
    /// Binds both sockets and exports `HYPRLAND_INSTANCE_SIGNATURE` for
    /// children. Failing to bind is logged; the compositor runs without IPC.
    pub fn new(handle: &LoopHandle<'static, State>) -> Ipc {
        let signature = format!("dd_{}", std::process::id());
        // SAFETY: called while the compositor starts, before it spawns any
        // thread that reads the environment.
        unsafe { std::env::set_var("HYPRLAND_INSTANCE_SIGNATURE", &signature) };
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let dir = runtime.join("hypr").join(&signature);
        let bound = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
            for name in [".socket.sock", ".socket2.sock"] {
                let path = dir.join(name);
                let _ = std::fs::remove_file(&path);
                if path.as_os_str().len() > 103 {
                    tracing::warn!("ipc: {} is longer than macOS allows for a socket", path.display());
                }
                let listener = UnixListener::bind(&path)?;
                listener.set_nonblocking(true)?;
                let events = name == ".socket2.sock";
                handle
                    .insert_source(Generic::new(listener, Interest::READ, Mode::Level), move |_, listener, state| {
                        while let Ok((stream, _)) = listener.accept() {
                            if events {
                                if stream.set_nonblocking(true).is_ok() {
                                    state.ipc.subscribers.push(stream);
                                }
                            } else {
                                state.ipc_answer(stream);
                            }
                        }
                        Ok(PostAction::Continue)
                    })
                    .map_err(|e| std::io::Error::other(e.to_string()))?;
            }
            Ok(())
        })();
        let dir = match bound {
            Ok(()) => {
                tracing::info!("ipc: Hyprland sockets in {}", dir.display());
                Some(dir)
            }
            Err(err) => {
                tracing::error!("ipc: {}: {err}", dir.display());
                None
            }
        };
        Ipc { dir, subscribers: Vec::new(), last: Snapshot::default() }
    }

    fn emit(&mut self, lines: &[String]) {
        if lines.is_empty() || self.subscribers.is_empty() {
            return;
        }
        let mut text = String::new();
        for line in lines {
            tracing::trace!("ipc event {line}");
            text.push_str(line);
            text.push('\n');
        }
        // A subscriber that can't keep up or has gone is dropped.
        self.subscribers.retain_mut(|s| s.write_all(text.as_bytes()).is_ok());
    }
}

// ---- window identity ----

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

struct IpcId(u64);

/// A window's address: a number given the first time IPC sees it.
fn window_id(window: &Window) -> u64 {
    window.user_data().insert_if_missing(|| IpcId(0x5000 + NEXT_ID.fetch_add(1, Ordering::Relaxed)));
    window.user_data().get::<IpcId>().unwrap().0
}

/// `app_id` and title.
pub fn class_title(window: &Window) -> (String, String) {
    let Some(surface) = window.wl_surface() else { return Default::default() };
    with_states(&surface, |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .map(|d| {
                let d = d.lock().unwrap();
                (d.app_id.clone().unwrap_or_default(), d.title.clone().unwrap_or_default())
            })
            .unwrap_or_default()
    })
}

// ---- snapshots and events ----

#[derive(Clone, Default, PartialEq)]
struct WindowInfo {
    workspace: i32,
    workspace_name: String,
    class: String,
    title: String,
}

#[derive(Clone, Default, PartialEq)]
struct Snapshot {
    /// Id to name.
    workspaces: BTreeMap<i32, String>,
    active: i32,
    /// The special workspace shown, by name.
    special: Option<String>,
    windows: BTreeMap<u64, WindowInfo>,
    focused: Option<u64>,
    fullscreen: bool,
    layout: String,
}

/// Hyprland's event data escapes nothing; commas in titles stay as they are.
fn events(old: &Snapshot, new: &Snapshot) -> Vec<String> {
    let mut out = Vec::new();
    for (id, name) in new.workspaces.iter().filter(|(id, _)| !old.workspaces.contains_key(id)) {
        out.push(format!("createworkspace>>{name}"));
        out.push(format!("createworkspacev2>>{id},{name}"));
    }
    if new.active != old.active {
        let id = new.active;
        out.push(format!("workspace>>{id}"));
        out.push(format!("workspacev2>>{id},{id}"));
    }
    if new.special != old.special {
        out.push(format!("activespecial>>{},{MONITOR}", new.special.as_deref().unwrap_or("")));
    }
    for (addr, w) in &new.windows {
        match old.windows.get(addr) {
            None => out.push(format!("openwindow>>{addr:x},{},{},{}", w.workspace_name, w.class, w.title)),
            Some(before) => {
                if before.workspace != w.workspace {
                    out.push(format!("movewindow>>{addr:x},{}", w.workspace_name));
                    out.push(format!("movewindowv2>>{addr:x},{},{}", w.workspace, w.workspace_name));
                }
                if before.title != w.title {
                    out.push(format!("windowtitle>>{addr:x}"));
                    out.push(format!("windowtitlev2>>{addr:x},{}", w.title));
                }
            }
        }
    }
    for addr in old.windows.keys().filter(|a| !new.windows.contains_key(a)) {
        out.push(format!("closewindow>>{addr:x}"));
    }
    let focused_title = |s: &Snapshot| s.focused.and_then(|a| s.windows.get(&a)).map(|w| (w.class.clone(), w.title.clone()));
    if new.focused != old.focused || focused_title(new) != focused_title(old) {
        let (class, title) = focused_title(new).unwrap_or_default();
        out.push(format!("activewindow>>{class},{title}"));
        out.push(match new.focused {
            Some(a) => format!("activewindowv2>>{a:x}"),
            None => "activewindowv2>>".into(),
        });
    }
    if new.fullscreen != old.fullscreen {
        out.push(format!("fullscreen>>{}", new.fullscreen as u8));
    }
    if new.layout != old.layout && !old.layout.is_empty() {
        out.push(format!("activelayout>>darwin-keyboard,{}", new.layout));
    }
    for (id, name) in old.workspaces.iter().filter(|(id, _)| !new.workspaces.contains_key(id)) {
        out.push(format!("destroyworkspace>>{name}"));
        out.push(format!("destroyworkspacev2>>{id},{name}"));
    }
    out
}

// ---- JSON ----

fn js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

impl State {
    /// Workspaces as Hyprland lists them: the ones on screen and any with windows.
    fn ipc_workspaces(&self) -> Vec<i32> {
        self.wm.workspaces.iter().filter(|(id, ws)| self.wm.visible(**id) || !ws.is_empty()).map(|(id, _)| *id).collect()
    }

    fn ipc_snapshot(&mut self) -> Snapshot {
        let mut windows = BTreeMap::new();
        for (id, ws) in &self.wm.workspaces {
            for w in ws.windows() {
                let (class, title) = class_title(&w);
                let workspace_name = self.wm.workspace_name(*id);
                windows.insert(window_id(&w), WindowInfo { workspace: *id, workspace_name, class, title });
            }
        }
        Snapshot {
            workspaces: self.ipc_workspaces().into_iter().map(|id| (id, self.wm.workspace_name(id))).collect(),
            active: self.wm.active,
            special: self.wm.special.map(|id| self.wm.workspace_name(id)),
            windows,
            focused: self.focused_window().map(|w| window_id(&w)),
            fullscreen: self.fullscreen_active(),
            layout: self.layout_name(),
        }
    }

    fn layout_name(&mut self) -> String {
        let Some(keyboard) = self.seat.get_keyboard() else { return String::new() };
        keyboard.with_xkb_state(self, |ctx| {
            let xkb = ctx.xkb().lock().unwrap();
            xkb.layout_name(xkb.active_layout()).to_string()
        })
    }

    /// Once a frame: send the events for whatever changed.
    pub fn ipc_tick(&mut self) {
        let now = self.ipc_snapshot();
        if now != self.ipc.last {
            let lines = events(&self.ipc.last, &now);
            self.ipc.emit(&lines);
            self.ipc.last = now;
        }
    }

    fn workspace_json(&self, id: i32) -> String {
        let ws = self.wm.workspaces.get(&id);
        let windows = ws.map(|ws| ws.windows()).unwrap_or_default();
        let last = ws.and_then(|ws| ws.last_focused());
        let (addr, title) = match &last {
            Some(w) => (format!("0x{:x}", window_id(w)), class_title(w).1),
            None => ("0x0".into(), String::new()),
        };
        format!(
            r#"{{"id":{id},"name":{name},"monitor":{MONITOR:?},"monitorID":0,"windows":{n},"hasfullscreen":{fs},"lastwindow":{addr},"lastwindowtitle":{title},"ispersistent":false}}"#,
            name = js(&self.wm.workspace_name(id)),
            n = windows.len(),
            fs = ws.is_some_and(|ws| ws.fullscreen.is_some()),
            addr = js(&addr),
            title = js(&title),
        )
    }

    fn client_json(&self, window: &Window) -> String {
        let (class, title) = class_title(window);
        let ws = self.wm.workspaces.iter().find(|(_, ws)| ws.windows().contains(window)).map(|(id, ws)| (*id, ws));
        let rect = crate::anim::with_anim(window, |a| *a.rect.goal()).unwrap_or_default();
        let pid = window
            .wl_surface()
            .and_then(|s| self.display_handle.get_client(s.id()).ok())
            .and_then(|c| c.get_credentials(&self.display_handle).ok())
            .map(|c| c.pid)
            .unwrap_or(-1);
        let grouped = ws
            .and_then(|(_, ws)| ws.group_of(window))
            .map(|g| g.members.iter().map(|m| js(&format!("0x{:x}", window_id(m)))).collect::<Vec<_>>().join(","))
            .unwrap_or_default();
        let (ws_id, floating, fullscreen, history) = match ws {
            Some((id, ws)) => (
                id,
                ws.floating.contains(window),
                ws.fullscreen.as_ref() == Some(window),
                ws.focus_history_id(window),
            ),
            None => (-1, false, false, -1),
        };
        format!(
            r#"{{"address":{addr},"mapped":true,"hidden":false,"at":[{x},{y}],"size":[{w},{h}],"workspace":{{"id":{ws_id},"name":{ws_name}}},"floating":{floating},"pseudo":false,"monitor":0,"class":{class},"title":{title},"initialClass":{class},"initialTitle":{title},"pid":{pid},"xwayland":false,"pinned":false,"fullscreen":{fs},"fullscreenClient":{fs},"grouped":[{grouped}],"tags":[],"swallowing":"0x0","focusHistoryID":{history},"inhibitingIdle":false,"xdgTag":"","xdgDescription":"","contentType":"none"}}"#,
            addr = js(&format!("0x{:x}", window_id(window))),
            x = rect.x.round() as i32,
            y = rect.y.round() as i32,
            w = rect.w.round() as i32,
            h = rect.h.round() as i32,
            ws_name = js(&self.wm.workspace_name(ws_id)),
            class = js(&class),
            title = js(&title),
            fs = if fullscreen { 2 } else { 0 },
        )
    }

    fn monitor_json(&self) -> String {
        let zone = smithay::desktop::layer_map_for_output(&self.output).non_exclusive_zone();
        let (w, h) = (self.output_size.w, self.output_size.h);
        let reserved = [zone.loc.x, zone.loc.y, w - zone.loc.x - zone.size.w, h - zone.loc.y - zone.size.h];
        let scale = self.output_scale;
        let mode = self.output.current_mode();
        let (pw, ph) = mode.map(|m| (m.size.w, m.size.h)).unwrap_or((w, h));
        let refresh = mode.map(|m| m.refresh as f64 / 1000.0).unwrap_or(60.0);
        format!(
            r#"{{"id":0,"name":{MONITOR:?},"description":"Apple window","make":"Apple","model":"window","serial":"","width":{pw},"height":{ph},"physicalWidth":0,"physicalHeight":0,"refreshRate":{refresh:.5},"x":0,"y":0,"activeWorkspace":{{"id":{a},"name":{an}}},"specialWorkspace":{{"id":{sp},"name":{spn}}},"reserved":[{r0},{r1},{r2},{r3}],"scale":{scale:.2},"transform":0,"focused":true,"dpmsStatus":true,"vrr":false,"solitary":"0","activelyTearing":false,"directScanoutTo":"0","disabled":false,"currentFormat":"XRGB8888","mirrorOf":"none","availableModes":[]}}"#,
            a = self.wm.active,
            an = js(&self.wm.active.to_string()),
            sp = self.wm.special.unwrap_or(0),
            spn = js(&self.wm.special.map(|id| self.wm.workspace_name(id)).unwrap_or_default()),
            r0 = reserved[0],
            r1 = reserved[1],
            r2 = reserved[2],
            r3 = reserved[3],
        )
    }

    /// What Lua's `hl.get_*` queries see (`config::World`).
    pub fn lua_world(&self) -> crate::config::World {
        use crate::config::{WindowView, WorkspaceView, World};
        let mut windows = Vec::new();
        let mut workspaces = Vec::new();
        for (id, ws) in &self.wm.workspaces {
            for w in ws.windows() {
                let (class, title) = class_title(&w);
                let rect = crate::anim::with_anim(&w, |a| *a.rect.goal()).unwrap_or_default();
                let pid = w
                    .wl_surface()
                    .and_then(|s| self.display_handle.get_client(s.id()).ok())
                    .and_then(|c| c.get_credentials(&self.display_handle).ok())
                    .map(|c| c.pid as i64)
                    .unwrap_or(-1);
                windows.push(WindowView {
                    address: format!("0x{:x}", window_id(&w)),
                    class,
                    title,
                    workspace: *id,
                    floating: ws.floating.contains(&w),
                    fullscreen: ws.fullscreen.as_ref() == Some(&w),
                    at: (rect.x, rect.y),
                    size: (rect.w, rect.h),
                    pid,
                    focus_history_id: ws.focus_history_id(&w),
                    group: ws.group_of(&w).map(|g| crate::config::GroupView {
                        current_index: g.current + 1,
                        locked: g.locked,
                        members: g.members.iter().map(|m| format!("0x{:x}", window_id(m))).collect(),
                    }),
                });
            }
            if self.wm.visible(*id) || !ws.is_empty() {
                workspaces.push(WorkspaceView {
                    id: *id,
                    name: self.wm.workspace_name(*id),
                    windows: ws.windows().len(),
                    has_fullscreen: ws.fullscreen.is_some(),
                });
            }
        }
        World {
            windows,
            workspaces,
            active: self.wm.active,
            special: self.wm.special,
            focused: self.focused_window().map(|w| format!("0x{:x}", window_id(&w))),
            cursor: (self.pointer_location.x, self.pointer_location.y),
        }
    }

    /// Runs a bind with the current state visible to its Lua (`hl.get_*`).
    pub fn run_bind(&mut self, hit: crate::config::BindHit) {
        self.config.set_world(self.lua_world());
        for action in self.config.run_bind(hit) {
            self.run_action(action);
        }
    }

    /// Answers one request on `.socket.sock`.
    fn ipc_answer(&mut self, mut stream: UnixStream) {
        // Clients write the request right after connecting: a short blocking read.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
        let mut buf = vec![0u8; 8192];
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(err) => {
                tracing::debug!("ipc: reading a request: {err}");
                return;
            }
        };
        let request = String::from_utf8_lossy(&buf[..n]).into_owned();
        let reply = self.ipc_request(&request);
        tracing::debug!(request, reply_len = reply.len(), "ipc request");
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        let _ = stream.write_all(reply.as_bytes());
    }

    pub fn ipc_request(&mut self, request: &str) -> String {
        let request = request.trim();
        // "[flags]/command": flags are letters before the first slash.
        let command = match request.split_once('/') {
            Some((flags, rest)) if flags.chars().all(|c| c.is_ascii_alphabetic()) && !flags.contains(' ') => rest,
            _ => request,
        };
        let (name, args) = command.split_once(' ').map(|(n, a)| (n, a.trim())).unwrap_or((command, ""));
        match name {
            "monitors" => format!("[{}]", self.monitor_json()),
            "workspaces" => {
                let list: Vec<String> = self.ipc_workspaces().into_iter().map(|id| self.workspace_json(id)).collect();
                format!("[{}]", list.join(","))
            }
            "activeworkspace" => self.workspace_json(self.wm.active),
            "clients" => {
                let windows: Vec<Window> = self.wm.workspaces.values().flat_map(|ws| ws.windows()).collect();
                let list: Vec<String> = windows.iter().map(|w| self.client_json(w)).collect();
                format!("[{}]", list.join(","))
            }
            "activewindow" => self.focused_window().map(|w| self.client_json(&w)).unwrap_or_else(|| "{}".into()),
            "workspacerules" => "[]".into(),
            "devices" => {
                let layout = self.layout_name();
                format!(
                    r#"{{"mice":[],"keyboards":[{{"address":"0x1","name":"darwin-keyboard","rules":"","model":"","layout":{l},"variant":"","options":"","active_keymap":{l},"capsLock":false,"numLock":false,"main":true}}],"tablets":[],"touch":[],"switches":[]}}"#,
                    l = js(&layout)
                )
            }
            "version" => format!(
                r#"{{"branch":"","commit":"","version":{v},"dirty":false,"commit_message":"darwin-desktop","commit_date":"","tag":{t},"commits":"","buildAquamarine":"","buildHyprlang":"","buildHyprutils":"","buildHyprcursor":"","buildHyprgraphics":"","systemHyprlang":"","systemHyprutils":"","systemHyprcursor":"","systemHyprgraphics":"","abiHash":"","flags":[]}}"#,
                v = js(VERSION),
                t = js(&format!("v{VERSION}"))
            ),
            "systeminfo" => format!("Hyprland {VERSION} (darwin-desktop compatibility)\nconfigProvider: lua\n"),
            "dispatch" => self.ipc_dispatch(args),
            _ => {
                tracing::debug!("ipc: unsupported request {request:?}");
                "unknown request".into()
            }
        }
    }

    /// `dispatch` with a Lua expression (`hl.dsp.focus({...})`) or a legacy
    /// `dispatcher args` pair.
    fn ipc_dispatch(&mut self, args: &str) -> String {
        let actions = if args.starts_with("hl.") {
            self.config.set_world(self.lua_world());
            match self.config.eval_dispatch(args) {
                Ok(actions) => actions,
                Err(err) => return format!("error: {err}"),
            }
        } else {
            let (dispatcher, arg) = args.split_once(' ').map(|(d, a)| (d, a.trim())).unwrap_or((args, ""));
            let workspace = || WorkspaceTarget::parse(arg);
            let action = match dispatcher {
                "workspace" | "focusworkspaceoncurrentmonitor" => workspace().map(Action::FocusWorkspace),
                "movetoworkspace" => workspace().map(|target| Action::MoveToWorkspace { target, follow: true }),
                "movetoworkspacesilent" => workspace().map(|target| Action::MoveToWorkspace { target, follow: false }),
                "exec" => Some(Action::Exec(arg.to_string())),
                "togglegroup" => Some(Action::ToggleGroup),
                "changegroupactive" => Some(Action::GroupCycle(if arg == "b" { -1 } else { 1 })),
                "movegroupwindow" => Some(Action::GroupMoveWindow { forward: arg != "b" }),
                "lockactivegroup" => Some(Action::LockGroup(match arg {
                    "lock" => Some(true),
                    "unlock" => Some(false),
                    _ => None,
                })),
                "movewindoworgroup" => crate::binds::Direction::parse(arg).map(Action::MoveWindowOrGroup),
                "togglespecialworkspace" => {
                    Some(Action::ToggleSpecial(if arg.is_empty() { "special".into() } else { arg.to_string() }))
                }
                "killactive" => Some(Action::Close),
                "togglefloating" => Some(Action::ToggleFloat),
                "fullscreen" => Some(Action::Fullscreen),
                "togglesplit" => Some(Action::ToggleSplit),
                "exit" => Some(Action::Exit),
                _ => None,
            };
            match action {
                Some(a) => vec![a],
                None => return format!("Invalid dispatcher {dispatcher:?}"),
            }
        };
        for action in actions {
            self.run_action(action);
        }
        "ok".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_switch_events() {
        let mut a = Snapshot { active: 1, ..Default::default() };
        a.workspaces.insert(1, "1".into());
        a.windows.insert(0x10, WindowInfo { workspace: 1, workspace_name: "1".into(), class: "vte".into(), title: "sh".into() });
        a.focused = Some(0x10);
        let mut b = Snapshot { active: 2, ..Default::default() };
        b.workspaces.insert(1, "1".into());
        b.workspaces.insert(2, "2".into());
        b.windows = a.windows.clone();
        let ev = events(&a, &b);
        assert_eq!(
            ev,
            [
                "createworkspace>>2",
                "createworkspacev2>>2,2",
                "workspace>>2",
                "workspacev2>>2,2",
                "activewindow>>,",
                "activewindowv2>>",
            ]
        );
    }

    #[test]
    fn window_events() {
        let a = Snapshot { active: 1, ..Default::default() };
        let mut b = a.clone();
        b.windows.insert(0xab, WindowInfo { workspace: 1, workspace_name: "1".into(), class: "vte".into(), title: "sh".into() });
        b.focused = Some(0xab);
        assert_eq!(events(&a, &b), ["openwindow>>ab,1,vte,sh", "activewindow>>vte,sh", "activewindowv2>>ab"]);
        let mut c = b.clone();
        c.windows.get_mut(&0xab).unwrap().title = "vim".into();
        assert_eq!(events(&b, &c), ["windowtitle>>ab", "windowtitlev2>>ab,vim", "activewindow>>vte,vim", "activewindowv2>>ab"]);
        assert_eq!(events(&c, &a)[0], "closewindow>>ab");
    }

    #[test]
    fn special_workspace_events() {
        let a = Snapshot { active: 1, ..Default::default() };
        let mut b = a.clone();
        b.workspaces.insert(-98, "special:magic".into());
        b.special = Some("special:magic".into());
        assert_eq!(
            events(&a, &b),
            ["createworkspace>>special:magic", "createworkspacev2>>-98,special:magic", "activespecial>>special:magic,darwin-0"]
        );
        assert_eq!(events(&b, &a)[0], "activespecial>>,darwin-0");
    }

    #[test]
    fn json_strings() {
        assert_eq!(js("a\"b\\c\n"), r#""a\"b\\c\n""#);
    }
}
