//! Hyprland's Lua config (`hyprland.lua`, Hyprland 0.55+), run in an embedded
//! Lua 5.5 (mlua, vendored).
//!
//! The `hl` API is implemented as far as the compositor has the features:
//! `hl.config`, `hl.bind` with the `hl.dsp.*` dispatchers in `binds::Action`,
//! `hl.exec_cmd`, `hl.dispatch`, `hl.env`, `hl.on("hyprland.start")`,
//! `hl.curve`, `hl.animation`. Everything else in `hl` (window rules,
//! monitors, gestures, ...) is accepted, warned about once, and ignored, so a
//! full Hyprland config still loads. SUPER is Command.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    rc::Rc,
};

use mlua::{Function, Lua, MultiValue, Table, UserData, Value};

use crate::binds::{Action, Bind, BindRun, Direction, WorkspaceTarget, parse_chord};

/// Built in, used when there is no config file.
const DEFAULT_CONFIG: &str = include_str!("default-config.lua");

#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<ConfigValue>),
}

impl ConfigValue {
    fn as_f64(&self) -> Option<f64> {
        match self {
            ConfigValue::Int(i) => Some(*i as f64),
            ConfigValue::Float(f) => Some(*f),
            ConfigValue::Bool(b) => Some(*b as i64 as f64),
            ConfigValue::Str(s) => s.trim().parse().ok(),
            ConfigValue::List(_) => None,
        }
    }
}

/// CSS-style gaps: top, right, bottom, left (in points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gaps {
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub left: f64,
}

impl Gaps {
    pub fn all(v: f64) -> Gaps {
        Gaps { top: v, right: v, bottom: v, left: v }
    }

    fn parse(v: &ConfigValue) -> Option<Gaps> {
        if let Some(n) = v.as_f64() {
            return Some(Gaps::all(n));
        }
        let ConfigValue::Str(s) = v else { return None };
        let n: Vec<f64> = s.split([' ', ',']).filter(|p| !p.is_empty()).map(|p| p.parse().ok()).collect::<Option<_>>()?;
        Some(match n.as_slice() {
            [a] => Gaps::all(*a),
            [v, h] => Gaps { top: *v, right: *h, bottom: *v, left: *h },
            [t, h, b] => Gaps { top: *t, right: *h, bottom: *b, left: *h },
            [t, r, b, l] => Gaps { top: *t, right: *r, bottom: *b, left: *l },
            _ => return None,
        })
    }
}

#[derive(Clone, Debug)]
pub struct DwindleConfig {
    pub preserve_split: bool,
    /// 0: by pointer position, 1: new window first (left/top), 2: new window second.
    pub force_split: i64,
    pub default_split_ratio: f64,
    pub split_width_multiplier: f64,
    pub use_active_for_splits: bool,
}

/// An `hl.curve` (used by the animations, next).
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum Curve {
    Bezier([(f64, f64); 2]),
    Spring { mass: f64, stiffness: f64, damping: f64 },
}

/// An `hl.animation` leaf.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub speed: f64,
    pub curve: Option<String>,
    pub style: Option<String>,
}

/// The settings the compositor uses, with Hyprland's defaults. (Rounding and
/// the animation settings are read by the renderer, next.)
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Settings {
    pub gaps_in: Gaps,
    pub gaps_out: Gaps,
    pub border_size: i32,
    pub rounding: i32,
    pub layout: String,
    pub dwindle: DwindleConfig,
    /// 0: click to focus, 1+: keyboard focus follows the pointer.
    pub follow_mouse: i64,
    pub animations_enabled: bool,
    pub curves: BTreeMap<String, Curve>,
    pub animations: BTreeMap<String, AnimationConfig>,
}

impl Settings {
    fn from_flat(flat: &BTreeMap<String, ConfigValue>) -> Settings {
        let num = |k: &str, d: f64| flat.get(k).and_then(ConfigValue::as_f64).unwrap_or(d);
        let gaps = |k: &str, d: f64| flat.get(k).and_then(Gaps::parse).unwrap_or(Gaps::all(d));
        let string = |k: &str, d: &str| match flat.get(k) {
            Some(ConfigValue::Str(s)) => s.clone(),
            _ => d.to_string(),
        };
        Settings {
            gaps_in: gaps("general.gaps_in", 5.0),
            gaps_out: gaps("general.gaps_out", 20.0),
            border_size: num("general.border_size", 1.0) as i32,
            rounding: num("decoration.rounding", 0.0) as i32,
            layout: string("general.layout", "dwindle"),
            dwindle: DwindleConfig {
                preserve_split: num("dwindle.preserve_split", 0.0) != 0.0,
                force_split: num("dwindle.force_split", 0.0) as i64,
                default_split_ratio: num("dwindle.default_split_ratio", 1.0).clamp(0.1, 1.9),
                split_width_multiplier: num("dwindle.split_width_multiplier", 1.0),
                use_active_for_splits: num("dwindle.use_active_for_splits", 1.0) != 0.0,
            },
            follow_mouse: num("input.follow_mouse", 1.0) as i64,
            animations_enabled: num("animations.enabled", 1.0) != 0.0,
            curves: BTreeMap::new(),
            animations: BTreeMap::new(),
        }
    }
}

/// What the Lua side accumulates.
#[derive(Default)]
struct Shared {
    flat: BTreeMap<String, ConfigValue>,
    binds: Vec<Bind>,
    env: Vec<(String, String)>,
    start_hooks: Vec<Function>,
    /// Actions from `hl.exec_cmd` / `hl.dispatch`, run by the compositor.
    queue: Vec<Action>,
    curves: BTreeMap<String, Curve>,
    animations: BTreeMap<String, AnimationConfig>,
    warned: HashSet<String>,
}

impl Shared {
    fn warn_once(&mut self, what: &str) {
        if self.warned.insert(what.to_string()) {
            tracing::warn!("config: {what} is not supported yet; ignored");
        }
    }
}

/// A dispatcher value (`hl.dsp.*`), passed to `hl.bind` or `hl.dispatch`.
#[derive(Clone)]
struct Dsp(Action);
impl UserData for Dsp {}

/// The handle `hl.bind` and the ignored API calls return.
#[derive(Clone)]
struct Handle(Option<Rc<Cell<bool>>>);
impl UserData for Handle {
    fn add_methods<M: mlua::UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("set_enabled", |_, this, on: bool| {
            if let Some(flag) = &this.0 {
                flag.set(on);
            }
            Ok(())
        });
        methods.add_method("is_enabled", |_, this, ()| Ok(this.0.as_ref().is_none_or(|f| f.get())));
    }
}

pub struct Config {
    lua: Lua,
    shared: Rc<RefCell<Shared>>,
    pub settings: Settings,
    #[allow(dead_code)]
    pub path: Option<PathBuf>,
}

/// `~/.config/hypr/hyprland.lua` (XDG_CONFIG_HOME honoured).
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("hypr").join("hyprland.lua"))
}

impl Config {
    /// Loads the config file, or the built-in default when there is none.
    /// Errors are logged; whatever ran before the error stays in effect.
    pub fn load(path: Option<&Path>) -> Config {
        let lua = Lua::new();
        let shared = Rc::new(RefCell::new(Shared::default()));
        if let Err(err) = install_api(&lua, &shared) {
            tracing::error!("config: setting up the hl API: {err}");
        }
        let path = path.map(Path::to_path_buf).or_else(default_path).filter(|p| p.exists());
        let (source, name) = match &path {
            Some(p) => match std::fs::read_to_string(p) {
                Ok(s) => (s, p.display().to_string()),
                Err(err) => {
                    tracing::error!("config: reading {}: {err}; using the default", p.display());
                    (DEFAULT_CONFIG.to_string(), "default config".to_string())
                }
            },
            None => (DEFAULT_CONFIG.to_string(), "default config".to_string()),
        };
        if let Some(dir) = path.as_ref().and_then(|p| p.parent()) {
            // `require("myColors")` next to the config, as Hyprland does.
            let set = lua.globals().get::<Table>("package").and_then(|pkg| {
                let old: String = pkg.get("path")?;
                pkg.set("path", format!("{0}/?.lua;{0}/?/init.lua;{old}", dir.display()))
            });
            if let Err(err) = set {
                tracing::warn!("config: package.path: {err}");
            }
        }
        tracing::info!("config: loading {name}");
        if let Err(err) = lua.load(&source).set_name(&name).exec() {
            tracing::error!("config: {err}");
        }
        let settings = {
            let s = shared.borrow();
            let mut settings = Settings::from_flat(&s.flat);
            settings.curves = s.curves.clone();
            settings.animations = s.animations.clone();
            settings
        };
        tracing::info!("config: {} binds, layout {}", shared.borrow().binds.len(), settings.layout);
        Config { lua, shared, settings, path }
    }

    /// The bind for a chord, if one is enabled.
    pub fn find_bind(&self, mods: crate::binds::Mods, keysym: smithay::input::keyboard::Keysym) -> Option<BindHit> {
        let shared = self.shared.borrow();
        shared
            .binds
            .iter()
            .position(|b| b.enabled.get() && b.chord.mods == mods && b.chord.keysym == keysym)
            .map(BindHit)
    }

    /// Runs a bind: an action to perform, or a Lua function whose
    /// `hl.dispatch` / `hl.exec_cmd` calls queue actions.
    pub fn run_bind(&self, hit: BindHit) -> Vec<Action> {
        let func = {
            let shared = self.shared.borrow();
            match shared.binds.get(hit.0).map(|b| &b.run) {
                Some(BindRun::Action(a)) => return vec![a.clone()],
                Some(BindRun::Lua(f)) => f.clone(),
                None => return Vec::new(),
            }
        };
        if let Err(err) = func.call::<()>(()) {
            tracing::error!("config: bind function: {err}");
        }
        self.take_queue()
    }

    /// `hl.on("hyprland.start")` hooks, once the compositor is up.
    pub fn run_start_hooks(&self) -> Vec<Action> {
        let hooks: Vec<Function> = self.shared.borrow().start_hooks.clone();
        for hook in hooks {
            if let Err(err) = hook.call::<()>(()) {
                tracing::error!("config: start hook: {err}");
            }
        }
        self.take_queue()
    }

    /// Actions queued while the config ran (top-level `hl.exec_cmd`).
    pub fn take_queue(&self) -> Vec<Action> {
        std::mem::take(&mut self.shared.borrow_mut().queue)
    }

    /// `hl.env` variables, for spawned processes.
    pub fn env(&self) -> Vec<(String, String)> {
        self.shared.borrow().env.clone()
    }

    #[allow(dead_code)]
    pub fn lua(&self) -> &Lua {
        &self.lua
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BindHit(usize);

fn flatten(prefix: &str, table: &Table, out: &mut BTreeMap<String, ConfigValue>) -> mlua::Result<()> {
    for pair in table.pairs::<Value, Value>() {
        let (k, v) = pair?;
        let key = match k {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            _ => continue,
        };
        let path = if prefix.is_empty() { key } else { format!("{prefix}.{key}") };
        match v {
            Value::Table(t) if is_array(&t) => {
                out.insert(path, ConfigValue::List(t.sequence_values::<Value>().filter_map(|v| v.ok().and_then(scalar)).collect()));
            }
            Value::Table(t) => flatten(&path, &t, out)?,
            other => {
                if let Some(v) = scalar(other) {
                    out.insert(path, v);
                }
            }
        }
    }
    Ok(())
}

fn is_array(t: &Table) -> bool {
    t.raw_len() > 0
}

fn scalar(v: Value) -> Option<ConfigValue> {
    Some(match v {
        Value::Boolean(b) => ConfigValue::Bool(b),
        Value::Integer(i) => ConfigValue::Int(i),
        Value::Number(n) => ConfigValue::Float(n),
        Value::String(s) => ConfigValue::Str(s.to_string_lossy()),
        Value::Table(t) if is_array(&t) => {
            ConfigValue::List(t.sequence_values::<Value>().filter_map(|v| v.ok().and_then(scalar)).collect())
        }
        _ => return None,
    })
}

fn opt_str(t: &Table, key: &str) -> mlua::Result<Option<String>> {
    match t.get::<Value>(key)? {
        Value::String(s) => Ok(Some(s.to_str()?.to_string())),
        Value::Integer(i) => Ok(Some(i.to_string())),
        Value::Number(n) => Ok(Some(n.to_string())),
        _ => Ok(None),
    }
}

/// A table whose every field and call is accepted and ignored with one warning.
fn stub(lua: &Lua, shared: &Rc<RefCell<Shared>>, path: String) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    {
        let (shared, path) = (shared.clone(), path.clone());
        mt.set(
            "__index",
            lua.create_function(move |lua, (_t, key): (Table, String)| stub(lua, &shared, format!("{path}.{key}")))?,
        )?;
    }
    {
        let shared = shared.clone();
        mt.set(
            "__call",
            lua.create_function(move |lua, _args: MultiValue| {
                shared.borrow_mut().warn_once(&format!("{path}()"));
                lua.create_userdata(Handle(None))
            })?,
        )?;
    }
    t.set_metatable(Some(mt))?;
    Ok(t)
}

fn install_api(lua: &Lua, shared: &Rc<RefCell<Shared>>) -> mlua::Result<()> {
    let hl = lua.create_table()?;

    // hl.config({ section = { key = value } })
    {
        let shared = shared.clone();
        hl.set(
            "config",
            lua.create_function(move |_, t: Table| {
                let mut flat = BTreeMap::new();
                flatten("", &t, &mut flat)?;
                shared.borrow_mut().flat.extend(flat);
                Ok(())
            })?,
        )?;
    }

    // hl.bind("SUPER + Q", dispatcher | function, { options })
    {
        let shared = shared.clone();
        hl.set(
            "bind",
            lua.create_function(move |lua, (keys, run, _opts): (String, Value, Option<Table>)| {
                let enabled = Rc::new(Cell::new(true));
                let chord = match parse_chord(&keys) {
                    Ok(c) => c,
                    Err(err) => {
                        tracing::warn!("config: bind ignored: {err}");
                        return lua.create_userdata(Handle(Some(enabled)));
                    }
                };
                let run = match run {
                    Value::UserData(ud) if ud.is::<Dsp>() => BindRun::Action(ud.borrow::<Dsp>()?.0.clone()),
                    Value::Function(f) => BindRun::Lua(f),
                    _ => {
                        tracing::warn!("config: bind {keys:?}: unsupported dispatcher; ignored");
                        return lua.create_userdata(Handle(Some(enabled)));
                    }
                };
                shared.borrow_mut().binds.push(Bind { chord, run, enabled: enabled.clone() });
                lua.create_userdata(Handle(Some(enabled)))
            })?,
        )?;
    }

    // hl.exec_cmd(cmd), hl.dispatch(dsp)
    {
        let shared = shared.clone();
        hl.set(
            "exec_cmd",
            lua.create_function(move |_, cmd: String| {
                shared.borrow_mut().queue.push(Action::Exec(cmd));
                Ok(())
            })?,
        )?;
    }
    {
        let shared = shared.clone();
        hl.set(
            "dispatch",
            lua.create_function(move |_, d: mlua::AnyUserData| {
                let action = d.borrow::<Dsp>()?.0.clone();
                shared.borrow_mut().queue.push(action);
                Ok(())
            })?,
        )?;
    }

    // hl.env(name, value)
    {
        let shared = shared.clone();
        hl.set(
            "env",
            lua.create_function(move |_, (k, v): (String, String)| {
                shared.borrow_mut().env.push((k, v));
                Ok(())
            })?,
        )?;
    }

    // hl.on(event, fn): only "hyprland.start" for now.
    {
        let shared = shared.clone();
        hl.set(
            "on",
            lua.create_function(move |lua, (event, f): (String, Function)| {
                let mut s = shared.borrow_mut();
                if event == "hyprland.start" {
                    s.start_hooks.push(f);
                } else {
                    s.warn_once(&format!("hl.on({event:?})"));
                }
                lua.create_userdata(Handle(None))
            })?,
        )?;
    }

    // hl.curve(name, { type = "bezier", points = {{x1,y1},{x2,y2}} } | { type = "spring", ... })
    {
        let shared = shared.clone();
        hl.set(
            "curve",
            lua.create_function(move |_, (name, t): (String, Table)| {
                let kind: String = t.get::<Option<String>>("type")?.unwrap_or_else(|| "bezier".into());
                let curve = if kind == "spring" {
                    Curve::Spring {
                        mass: t.get::<Option<f64>>("mass")?.unwrap_or(1.0),
                        stiffness: t.get::<Option<f64>>("stiffness")?.unwrap_or(100.0),
                        damping: t.get::<Option<f64>>("damping")?.or(t.get::<Option<f64>>("dampening")?).unwrap_or(10.0),
                    }
                } else {
                    let pts: Vec<Vec<f64>> = t.get("points")?;
                    let p = |i: usize| pts.get(i).map(|p| (p.first().copied().unwrap_or(0.0), p.get(1).copied().unwrap_or(0.0)));
                    Curve::Bezier([p(0).unwrap_or((0.0, 0.0)), p(1).unwrap_or((1.0, 1.0))])
                };
                shared.borrow_mut().curves.insert(name, curve);
                Ok(())
            })?,
        )?;
    }

    // hl.animation({ leaf, enabled, speed, bezier | spring, style })
    {
        let shared = shared.clone();
        hl.set(
            "animation",
            lua.create_function(move |_, t: Table| {
                let leaf: String = t.get("leaf")?;
                let anim = AnimationConfig {
                    enabled: t.get::<Option<bool>>("enabled")?.unwrap_or(true),
                    speed: t.get::<Option<f64>>("speed")?.unwrap_or(10.0),
                    curve: opt_str(&t, "bezier")?.or(opt_str(&t, "spring")?),
                    style: opt_str(&t, "style")?,
                };
                shared.borrow_mut().animations.insert(leaf, anim);
                Ok(())
            })?,
        )?;
    }

    // Queries. No window/workspace objects yet: nil, so configs fall back
    // (e.g. `local w = hl.get_active_window(); if w and w.group then ...`).
    hl.set("get_active_window", lua.create_function(|_, _: MultiValue| Ok(Value::Nil))?)?;
    hl.set("get_active_workspace", lua.create_function(|_, _: MultiValue| Ok(Value::Nil))?)?;

    // hl.dsp: dispatcher constructors.
    let dsp = lua.create_table()?;
    let mk = |action: Action| -> mlua::Result<mlua::AnyUserData> { lua.create_userdata(Dsp(action)) };
    let _ = mk; // (closures below create their own)
    dsp.set("exec_cmd", lua.create_function(|lua, cmd: String| lua.create_userdata(Dsp(Action::Exec(cmd))))?)?;
    dsp.set("exit", lua.create_function(|lua, ()| lua.create_userdata(Dsp(Action::Exit)))?)?;
    dsp.set(
        "layout",
        lua.create_function(|lua, msg: String| {
            let action = match msg.trim() {
                "togglesplit" => Action::ToggleSplit,
                other => Action::Unsupported(format!("hl.dsp.layout({other:?})")),
            };
            lua.create_userdata(Dsp(action))
        })?,
    )?;
    dsp.set(
        "focus",
        lua.create_function(|lua, t: Table| {
            let action = if let Some(d) = opt_str(&t, "direction")?.as_deref().and_then(Direction::parse) {
                Action::Focus(d)
            } else if let Some(ws) = opt_str(&t, "workspace")?.as_deref().and_then(WorkspaceTarget::parse) {
                Action::FocusWorkspace(ws)
            } else {
                Action::Unsupported("hl.dsp.focus with these options".into())
            };
            lua.create_userdata(Dsp(action))
        })?,
    )?;
    let window = lua.create_table()?;
    window.set("close", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::Close)))?)?;
    window.set("kill", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::Close)))?)?;
    window.set("float", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::ToggleFloat)))?)?;
    window.set("fullscreen", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::Fullscreen)))?)?;
    window.set(
        "move",
        lua.create_function(|lua, t: Table| {
            let follow = t.get::<Option<bool>>("follow")?.unwrap_or(true);
            let action = if let Some(ws) = opt_str(&t, "workspace")?.as_deref().and_then(WorkspaceTarget::parse) {
                Action::MoveToWorkspace { target: ws, follow }
            } else if let Some(d) = opt_str(&t, "direction")?.as_deref().and_then(Direction::parse) {
                Action::MoveWindow(d)
            } else {
                Action::Unsupported("hl.dsp.window.move with these options".into())
            };
            lua.create_userdata(Dsp(action))
        })?,
    )?;
    // Unknown window dispatchers (pseudo, pin, drag, resize, ...).
    window.set_metatable(Some({
        let mt = lua.create_table()?;
        mt.set(
            "__index",
            lua.create_function(|lua, (_t, key): (Table, String)| {
                lua.create_function(move |lua, _: MultiValue| {
                    lua.create_userdata(Dsp(Action::Unsupported(format!("hl.dsp.window.{key}"))))
                })
            })?,
        )?;
        mt
    }))?;
    dsp.set("window", window)?;
    // Any other dispatcher group or function.
    dsp.set_metatable(Some({
        let mt = lua.create_table()?;
        mt.set(
            "__index",
            lua.create_function(|lua, (_t, key): (Table, String)| {
                let group = lua.create_table()?;
                let g = lua.create_table()?;
                let k = key.clone();
                g.set(
                    "__index",
                    lua.create_function(move |lua, (_t, sub): (Table, String)| {
                        let name = format!("hl.dsp.{k}.{sub}");
                        lua.create_function(move |lua, _: MultiValue| {
                            lua.create_userdata(Dsp(Action::Unsupported(name.clone())))
                        })
                    })?,
                )?;
                let name = format!("hl.dsp.{key}");
                g.set(
                    "__call",
                    lua.create_function(move |lua, _: MultiValue| {
                        lua.create_userdata(Dsp(Action::Unsupported(name.clone())))
                    })?,
                )?;
                group.set_metatable(Some(g))?;
                Ok(group)
            })?,
        )?;
        mt
    }))?;
    hl.set("dsp", dsp)?;

    // Everything else in `hl`: accepted and ignored.
    {
        let shared = shared.clone();
        let mt = lua.create_table()?;
        mt.set(
            "__index",
            lua.create_function(move |lua, (_t, key): (Table, String)| stub(lua, &shared, format!("hl.{key}")))?,
        )?;
        hl.set_metatable(Some(mt))?;
    }

    lua.globals().set("hl", hl)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_str(src: &str) -> Config {
        let dir = std::env::temp_dir().join(format!("compositor-config-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.lua", src.len()));
        std::fs::write(&path, src).unwrap();
        Config::load(Some(&path))
    }

    #[test]
    fn settings_and_binds() {
        let c = load_str(
            r#"
            local mainMod = "SUPER"
            hl.config({ general = { gaps_in = 3, gaps_out = "10 20", border_size = 2, layout = "dwindle" },
                        dwindle = { preserve_split = true } })
            hl.bind(mainMod .. " + Q", hl.dsp.exec_cmd("foot"))
            for i = 1, 3 do hl.bind(mainMod .. " + " .. i, hl.dsp.focus({ workspace = i })) end
            hl.bind(mainMod .. " + P", hl.dsp.window.pseudo())
            hl.window_rule({ name = "x", match = { class = ".*" } }):set_enabled(false)
            hl.monitor({ output = "", mode = "preferred" })
            hl.curve("quick", { type = "bezier", points = { {0.15, 0}, {0.1, 1} } })
            hl.animation({ leaf = "windows", speed = 4.79, bezier = "quick" })
            hl.exec_cmd("true")
        "#,
        );
        assert_eq!(c.settings.gaps_in, Gaps::all(3.0));
        assert_eq!(c.settings.gaps_out, Gaps { top: 10.0, right: 20.0, bottom: 10.0, left: 20.0 });
        assert_eq!(c.settings.border_size, 2);
        assert!(c.settings.dwindle.preserve_split);
        assert!(c.settings.curves.contains_key("quick"));
        assert_eq!(c.settings.animations["windows"].curve.as_deref(), Some("quick"));
        assert_eq!(c.shared.borrow().binds.len(), 5);
        assert!(matches!(c.take_queue().as_slice(), [Action::Exec(cmd)] if cmd == "true"));
    }

    #[test]
    fn default_config_loads() {
        let c = Config::load(Some(Path::new("/nonexistent/hyprland.lua")));
        assert!(!c.shared.borrow().binds.is_empty());
    }

    #[test]
    fn users_linux_config_loads() {
        // The project owner's Linux Hyprland config (docs/exampleconfig.lua).
        let c = load_str(include_str!("../../docs/exampleconfig.lua"));
        assert!(c.shared.borrow().binds.len() > 30);
        assert_eq!(c.settings.gaps_in, Gaps::all(1.0));
        assert_eq!(c.settings.border_size, 1);
        assert!(c.settings.curves.contains_key("easy"));
        // Its bind functions call hl.get_active_window(); they must run, not error.
        let hit = c
            .find_bind(crate::binds::Mods { logo: true, ..Default::default() }, crate::binds::parse_chord("h").unwrap().keysym)
            .expect("SUPER + h");
        let actions = c.run_bind(hit);
        assert!(matches!(actions.as_slice(), [Action::Focus(Direction::Left)]));
    }

    #[test]
    fn hyprland_example_loads() {
        // Hyprland's own example config (v0.57.2) must load without errors.
        let c = load_str(include_str!("../tests/hyprland-example.lua"));
        assert!(c.shared.borrow().binds.len() > 20);
        assert_eq!(c.settings.gaps_out, Gaps::all(20.0));
    }
}
