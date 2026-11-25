//! Hyprland's Lua config (`hyprland.lua`, Hyprland 0.55+), run in an embedded
//! Lua 5.5 (mlua, vendored).
//!
//! The `hl` API is implemented as far as the compositor has the features:
//! `hl.config`, `hl.bind` with the `hl.dsp.*` dispatchers in `binds::Action`,
//! `hl.exec_cmd`, `hl.dispatch`, `hl.env`, `hl.on("hyprland.start")`,
//! `hl.curve`, `hl.animation`, `hl.gesture` (workspace swipes),
//! `hl.window_rule` (`rules.rs`). Everything else in `hl` (layer rules,
//! monitors, ...) is accepted, warned about once, and ignored, so a
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

/// A premultiplied RGBA colour.
pub type Rgba = [f32; 4];

/// A border colour: one colour, or Hyprland's angled multi-colour gradient.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    pub colors: Vec<Rgba>,
    /// Degrees; 0 runs left to right, 90 top to bottom.
    pub angle: f32,
}

impl Gradient {
    pub fn solid(c: Rgba) -> Gradient {
        Gradient { colors: vec![c], angle: 0.0 }
    }
}

/// Hyprland's colour formats: `rgba(RRGGBBAA)`, `rgb(RRGGBB)`, `rgba(r, g, b, a)`
/// (a in 0-1), `0xAARRGGBB` (string or number). Returned premultiplied.
pub fn parse_color(v: &ConfigValue) -> Option<Rgba> {
    let argb = |n: u32| {
        let a = ((n >> 24) & 0xff) as f32 / 255.0;
        let r = ((n >> 16) & 0xff) as f32 / 255.0;
        let g = ((n >> 8) & 0xff) as f32 / 255.0;
        let b = (n & 0xff) as f32 / 255.0;
        [r * a, g * a, b * a, a]
    };
    match v {
        ConfigValue::Int(n) => Some(argb(*n as u32)),
        ConfigValue::Float(f) => Some(argb(*f as u32)),
        ConfigValue::Str(s) => {
            let s = s.trim();
            if let Some(hex) = s.strip_prefix("0x") {
                return u32::from_str_radix(hex, 16).ok().map(argb);
            }
            let (func, args) = s.split_once('(')?;
            let args = args.strip_suffix(')')?.trim();
            if args.contains(',') {
                let n: Vec<f32> = args.split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
                let a = if func == "rgba" { *n.get(3)? } else { 1.0 };
                return Some([n[0] / 255.0 * a, n.get(1)? / 255.0 * a, n.get(2)? / 255.0 * a, a]);
            }
            let n = u32::from_str_radix(args, 16).ok()?;
            match (func, args.len()) {
                ("rgba", 8) => Some(argb(n.rotate_right(8))),
                ("rgb", 6) => Some(argb(0xff00_0000 | n)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// 0xAARRGGBB to premultiplied RGBA.
fn argb_premultiplied(n: u32) -> Rgba {
    let a = ((n >> 24) & 0xff) as f32 / 255.0;
    let c = |shift: u32| ((n >> shift) & 0xff) as f32 / 255.0 * a;
    [c(16), c(8), c(0), a]
}

/// Premultiplied RGBA back to straight.
fn unpremultiply(c: Rgba) -> Rgba {
    if c[3] <= 0.0 { [0.0; 4] } else { [c[0] / c[3], c[1] / c[3], c[2] / c[3], c[3]] }
}

fn parse_gradient(flat: &BTreeMap<String, ConfigValue>, key: &str) -> Option<Gradient> {
    if let Some(v) = flat.get(key) {
        if let ConfigValue::List(items) = v {
            let colors: Vec<Rgba> = items.iter().filter_map(parse_color).collect();
            return (!colors.is_empty()).then_some(Gradient { colors, angle: 0.0 });
        }
        return parse_color(v).map(Gradient::solid);
    }
    let ConfigValue::List(items) = flat.get(&format!("{key}.colors"))? else { return None };
    let colors: Vec<Rgba> = items.iter().filter_map(parse_color).collect();
    let angle = flat.get(&format!("{key}.angle")).and_then(ConfigValue::as_f64).unwrap_or(0.0) as f32;
    (!colors.is_empty()).then_some(Gradient { colors, angle })
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

/// What `hl.get_*` queries see: the compositor's state when a bind (or an
/// IPC dispatch) started running. Lua can't reach the live state, so the
/// compositor hands it this snapshot first (`Config::set_world`).
#[derive(Clone, Debug, Default)]
pub struct World {
    pub windows: Vec<WindowView>,
    pub workspaces: Vec<WorkspaceView>,
    pub active: i32,
    pub special: Option<i32>,
    /// The focused window's address.
    pub focused: Option<String>,
    pub cursor: (f64, f64),
}

#[derive(Clone, Debug, Default)]
pub struct WindowView {
    pub address: String,
    pub class: String,
    pub title: String,
    pub workspace: i32,
    pub floating: bool,
    pub fullscreen: bool,
    pub at: (f64, f64),
    pub size: (f64, f64),
    pub pid: i64,
    pub focus_history_id: i32,
    pub group: Option<GroupView>,
}

/// A window's group, as `window.group` (Hyprland's `objects/LuaGroup.cpp`).
#[derive(Clone, Debug, Default)]
pub struct GroupView {
    /// 1-based, as Hyprland's Lua.
    pub current_index: usize,
    pub locked: bool,
    /// Addresses, in tab order.
    pub members: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct WorkspaceView {
    pub id: i32,
    pub name: String,
    pub windows: usize,
    pub has_fullscreen: bool,
}

/// An `hl.gesture`: a trackpad swipe with `fingers` fingers along
/// `direction` runs `action` (only "workspace" is supported).
#[derive(Clone, Debug)]
pub struct Gesture {
    pub fingers: u32,
    /// "horizontal" or "vertical" (also "left"/"right"/"up"/"down": their axis).
    pub direction: String,
    pub action: String,
}

/// The `gestures` section's workspace swipe options, Hyprland's defaults.
#[derive(Clone, Debug)]
pub struct SwipeConfig {
    /// How far (trackpad units, about Hyprland's) a full workspace swipe is.
    pub distance: f64,
    /// Release past this fraction to switch, else snap back.
    pub cancel_ratio: f64,
    /// Release faster than this (units per 10 ms) to switch regardless.
    pub min_speed_to_force: f64,
    /// Content follows the fingers (swiping left shows the next workspace).
    pub invert: bool,
    /// Swiping past the last workspace makes a new one.
    pub create_new: bool,
}

/// The `group` section, with Hyprland's defaults.
#[derive(Clone, Debug)]
pub struct GroupConfig {
    pub border_active: Gradient,
    pub border_inactive: Gradient,
    pub border_locked_active: Gradient,
    pub border_locked_inactive: Gradient,
    /// New windows join the focused (unlocked) group.
    pub auto_group: bool,
    pub insert_after_current: bool,
    pub bar: GroupbarConfig,
}

/// `group.groupbar`: the tab strip above a grouped window.
#[derive(Clone, Debug)]
pub struct GroupbarConfig {
    pub enabled: bool,
    pub disable_when_only: bool,
    pub font_family: Option<String>,
    pub font_size: f64,
    pub height: f64,
    pub indicator_height: f64,
    pub indicator_gap: f64,
    pub render_titles: bool,
    pub rounding: f64,
    pub gaps_in: f64,
    pub gaps_out: f64,
    pub keep_upper_gap: bool,
    pub text_offset: f64,
    pub text_padding: f64,
    /// Straight RGBA (not premultiplied: the text is drawn with them).
    pub text_color: [f32; 4],
    pub text_color_inactive: [f32; 4],
    pub text_color_locked_active: [f32; 4],
    pub text_color_locked_inactive: [f32; 4],
    /// Premultiplied, as the borders.
    pub col_active: [f32; 4],
    pub col_inactive: [f32; 4],
    pub col_locked_active: [f32; 4],
    pub col_locked_inactive: [f32; 4],
}

impl GroupbarConfig {
    /// The space the bar takes above a window (Hyprland's desiredExtents).
    pub fn reserved(&self) -> f64 {
        if !self.enabled {
            return 0.0;
        }
        self.gaps_out * (1.0 + self.keep_upper_gap as u8 as f64) + self.one_bar()
    }

    /// One bar: the indicator, its gap and the title strip.
    pub fn one_bar(&self) -> f64 {
        self.indicator_height + self.indicator_gap + if self.render_titles { self.height } else { 0.0 }
    }
}

/// `decoration.shadow`, with Hyprland's defaults.
#[derive(Clone, Debug)]
pub struct ShadowConfig {
    pub enabled: bool,
    /// Logical points.
    pub range: f64,
    /// Falloff exponent, 1 to 4.
    pub power: f64,
    pub sharp: bool,
    /// Premultiplied.
    pub color: Rgba,
    pub color_inactive: Option<Rgba>,
    pub offset: (f64, f64),
    pub scale: f64,
}

/// `decoration.blur`, with Hyprland's defaults.
#[derive(Clone, Debug)]
pub struct BlurConfig {
    pub enabled: bool,
    /// Sample distance, in pixels.
    pub size: f64,
    pub passes: u32,
    /// Floating windows blur only the background (wallpaper, bottom layers), not the tiles under them.
    pub xray: bool,
    pub noise: f64,
    pub contrast: f64,
    pub brightness: f64,
    pub vibrancy: f64,
    pub vibrancy_darkness: f64,
}

/// An `hl.curve`.
#[derive(Clone, Debug)]
pub enum Curve {
    Bezier([(f64, f64); 2]),
    Spring { mass: f64, stiffness: f64, damping: f64 },
}

/// An `hl.animation` leaf.
#[derive(Clone, Debug)]
pub struct AnimationConfig {
    pub enabled: bool,
    pub speed: f64,
    /// A bezier's name, or `spring:` and a spring's name (as Hyprland).
    pub curve: Option<String>,
    pub style: Option<String>,
}

/// The settings the compositor uses, with Hyprland's defaults.
#[derive(Clone, Debug)]
pub struct Settings {
    pub gaps_in: Gaps,
    pub gaps_out: Gaps,
    pub border_size: i32,
    pub active_border: Gradient,
    pub inactive_border: Gradient,
    pub rounding: i32,
    /// `decoration.dim_special`: how much the workspace under a special one darkens.
    pub dim_special: f64,
    pub group: GroupConfig,
    pub shadow: ShadowConfig,
    pub blur: BlurConfig,
    pub layout: String,
    /// `general.gaps_workspaces`: space between workspaces as they slide.
    pub workspace_gap: f64,
    pub dwindle: DwindleConfig,
    /// 0: click to focus, 1+: keyboard focus follows the pointer.
    pub follow_mouse: i64,
    /// `input.touchpad.scroll_factor`.
    pub touchpad_scroll_factor: f64,
    pub gestures: Vec<Gesture>,
    pub swipe: SwipeConfig,
    pub animations_enabled: bool,
    pub curves: BTreeMap<String, Curve>,
    pub animations: BTreeMap<String, AnimationConfig>,
}

impl Settings {
    pub(crate) fn from_flat(flat: &BTreeMap<String, ConfigValue>) -> Settings {
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
            // Hyprland's defaults: white and dark grey.
            active_border: parse_gradient(flat, "general.col.active_border")
                .unwrap_or(Gradient::solid([1.0, 1.0, 1.0, 1.0])),
            inactive_border: parse_gradient(flat, "general.col.inactive_border")
                .unwrap_or(Gradient::solid([0.267, 0.267, 0.267, 1.0])),
            rounding: num("decoration.rounding", 0.0) as i32,
            dim_special: num("decoration.dim_special", 0.2).clamp(0.0, 1.0),
            group: {
                let argb = |n: u32| Gradient::solid(argb_premultiplied(n));
                let grad = |k: &str, d: u32| parse_gradient(flat, k).unwrap_or_else(|| argb(d));
                let color = |k: &str, d: u32| grad(k, d).colors[0];
                let text = |k: &str| parse_gradient(flat, k).map(|g| unpremultiply(g.colors[0]));
                let text_color = text("group.groupbar.text_color").unwrap_or([1.0; 4]);
                let text_inactive = text("group.groupbar.text_color_inactive").unwrap_or(text_color);
                GroupConfig {
                    border_active: grad("group.col.border_active", 0x66ffff00),
                    border_inactive: grad("group.col.border_inactive", 0x66777700),
                    border_locked_active: grad("group.col.border_locked_active", 0x66775500),
                    border_locked_inactive: grad("group.col.border_locked_inactive", 0x66ff5500),
                    auto_group: num("group.auto_group", 1.0) != 0.0,
                    insert_after_current: num("group.insert_after_current", 1.0) != 0.0,
                    bar: GroupbarConfig {
                        enabled: num("group.groupbar.enabled", 1.0) != 0.0,
                        disable_when_only: num("group.groupbar.disable_when_only", 0.0) != 0.0,
                        font_family: match flat.get("group.groupbar.font_family") {
                            Some(ConfigValue::Str(s)) if !s.is_empty() && s != "[[EMPTY]]" => Some(s.clone()),
                            _ => None,
                        },
                        font_size: num("group.groupbar.font_size", 8.0).clamp(2.0, 64.0),
                        height: num("group.groupbar.height", 14.0).clamp(1.0, 64.0),
                        indicator_height: num("group.groupbar.indicator_height", 3.0).clamp(0.0, 64.0),
                        indicator_gap: num("group.groupbar.indicator_gap", 0.0).clamp(0.0, 64.0),
                        render_titles: num("group.groupbar.render_titles", 1.0) != 0.0,
                        rounding: num("group.groupbar.rounding", 1.0).clamp(0.0, 40.0),
                        gaps_in: num("group.groupbar.gaps_in", 2.0).clamp(0.0, 20.0),
                        gaps_out: num("group.groupbar.gaps_out", 2.0).clamp(0.0, 20.0),
                        keep_upper_gap: num("group.groupbar.keep_upper_gap", 1.0) != 0.0,
                        text_offset: num("group.groupbar.text_offset", 0.0),
                        text_padding: num("group.groupbar.text_padding", 0.0).max(0.0),
                        text_color,
                        text_color_inactive: text_inactive,
                        text_color_locked_active: text("group.groupbar.text_color_locked_active").unwrap_or(text_color),
                        text_color_locked_inactive: text("group.groupbar.text_color_locked_inactive").unwrap_or(text_inactive),
                        col_active: color("group.groupbar.col.active", 0x66ffff00),
                        col_inactive: color("group.groupbar.col.inactive", 0x66777700),
                        col_locked_active: color("group.groupbar.col.locked_active", 0x66ff5500),
                        col_locked_inactive: color("group.groupbar.col.locked_inactive", 0x66775500),
                    },
                }
            },
            shadow: {
                let color = |k: &str| parse_gradient(flat, k).and_then(|g| g.colors.first().copied());
                let offset = match flat.get("decoration.shadow.offset") {
                    Some(ConfigValue::List(v)) if v.len() == 2 => {
                        (v[0].as_f64().unwrap_or(0.0), v[1].as_f64().unwrap_or(0.0))
                    }
                    Some(ConfigValue::Str(s)) => {
                        let n: Vec<f64> = s.split([' ', ',']).filter_map(|p| p.parse().ok()).collect();
                        (n.first().copied().unwrap_or(0.0), n.get(1).copied().unwrap_or(0.0))
                    }
                    _ => (num("decoration.shadow.offset.x", 0.0), num("decoration.shadow.offset.y", 0.0)),
                };
                ShadowConfig {
                    enabled: num("decoration.shadow.enabled", 1.0) != 0.0,
                    range: num("decoration.shadow.range", 4.0).clamp(0.0, 100.0),
                    power: num("decoration.shadow.render_power", 3.0).clamp(1.0, 4.0),
                    sharp: num("decoration.shadow.sharp", 0.0) != 0.0,
                    // 0xee1a1a1a, premultiplied.
                    color: color("decoration.shadow.color")
                        .unwrap_or([0.102 * 0.933, 0.102 * 0.933, 0.102 * 0.933, 0.933]),
                    color_inactive: color("decoration.shadow.color_inactive"),
                    offset,
                    scale: num("decoration.shadow.scale", 1.0).clamp(0.0, 1.0),
                }
            },
            blur: BlurConfig {
                enabled: num("decoration.blur.enabled", 1.0) != 0.0,
                size: num("decoration.blur.size", 8.0).clamp(0.0, 100.0),
                passes: num("decoration.blur.passes", 1.0).clamp(0.0, 8.0) as u32,
                xray: num("decoration.blur.xray", 0.0) != 0.0,
                noise: num("decoration.blur.noise", 0.0117).clamp(0.0, 1.0),
                contrast: num("decoration.blur.contrast", 0.8916).clamp(0.0, 2.0),
                brightness: num("decoration.blur.brightness", 1.0).clamp(0.0, 2.0),
                vibrancy: num("decoration.blur.vibrancy", 0.1696).clamp(0.0, 1.0),
                vibrancy_darkness: num("decoration.blur.vibrancy_darkness", 0.0).clamp(0.0, 1.0),
            },
            layout: string("general.layout", "dwindle"),
            workspace_gap: num("general.gaps_workspaces", 0.0),
            dwindle: DwindleConfig {
                preserve_split: num("dwindle.preserve_split", 0.0) != 0.0,
                force_split: num("dwindle.force_split", 0.0) as i64,
                default_split_ratio: num("dwindle.default_split_ratio", 1.0).clamp(0.1, 1.9),
                split_width_multiplier: num("dwindle.split_width_multiplier", 1.0),
                use_active_for_splits: num("dwindle.use_active_for_splits", 1.0) != 0.0,
            },
            follow_mouse: num("input.follow_mouse", 1.0) as i64,
            touchpad_scroll_factor: num("input.touchpad.scroll_factor", 1.0).max(0.0),
            gestures: Vec::new(),
            swipe: SwipeConfig {
                distance: num("gestures.workspace_swipe_distance", 300.0).max(1.0),
                cancel_ratio: num("gestures.workspace_swipe_cancel_ratio", 0.5).clamp(0.0, 1.0),
                min_speed_to_force: num("gestures.workspace_swipe_min_speed_to_force", 30.0).max(0.0),
                invert: num("gestures.workspace_swipe_invert", 1.0) != 0.0,
                create_new: num("gestures.workspace_swipe_create_new", 1.0) != 0.0,
            },
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
    gestures: Vec<Gesture>,
    window_rules: Vec<crate::rules::WindowRule>,
    world: World,
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
            settings.gestures = s.gestures.clone();
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
            .position(|b| b.enabled.get() && b.chord.mouse.is_none() && b.chord.mods == mods && b.chord.keysym == keysym)
            .map(BindHit)
    }

    /// The bind for a mouse button or wheel chord, if one is enabled.
    pub fn find_mouse_bind(&self, mods: crate::binds::Mods, key: crate::binds::MouseKey) -> Option<BindHit> {
        let shared = self.shared.borrow();
        shared.binds.iter().position(|b| b.enabled.get() && b.chord.mods == mods && b.chord.mouse == Some(key)).map(BindHit)
    }

    /// The action a bind runs directly (not a Lua function).
    pub fn bind_action(&self, hit: &BindHit) -> Option<Action> {
        match &self.shared.borrow().binds[hit.0].run {
            BindRun::Action(a) => Some(a.clone()),
            BindRun::Lua(_) => None,
        }
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

    /// The window rules, in config order.
    pub fn window_rules(&self) -> std::cell::Ref<'_, [crate::rules::WindowRule]> {
        std::cell::Ref::map(self.shared.borrow(), |s| s.window_rules.as_slice())
    }

    /// The state `hl.get_*` queries answer from, until the next call.
    pub fn set_world(&self, world: World) {
        self.shared.borrow_mut().world = world;
    }

    /// Evaluates `hl.dispatch(<expr>)` in the config's Lua (IPC's Lua
    /// dispatch, e.g. `hl.dsp.focus({ workspace = "2" })`).
    pub fn eval_dispatch(&self, expr: &str) -> Result<Vec<Action>, String> {
        self.take_queue();
        self.lua.load(format!("hl.dispatch({expr})")).set_name("dispatch").exec().map_err(|e| e.to_string())?;
        Ok(self.take_queue())
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

/// `hl.get_*`: plain tables built from the `World` snapshot, with
/// Hyprland's field names (`objects/LuaWindow.cpp`, `LuaWorkspace.cpp`).
/// No groups yet: `window.group` is nil.
fn install_queries(lua: &Lua, hl: &Table, shared: &Rc<RefCell<Shared>>) -> mlua::Result<()> {
    fn monitor(lua: &Lua) -> mlua::Result<Table> {
        let m = lua.create_table()?;
        m.set("id", 0)?;
        m.set("name", "darwin-0")?;
        Ok(m)
    }
    fn workspace(lua: &Lua, world: &World, ws: &WorkspaceView) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        t.set("id", ws.id)?;
        t.set("name", ws.name.clone())?;
        t.set("special", ws.id < 0)?;
        t.set("windows", ws.windows)?;
        let visible = ws.id == world.active || Some(ws.id) == world.special;
        t.set("visible", visible)?;
        t.set("active", visible)?;
        t.set("has_fullscreen", ws.has_fullscreen)?;
        t.set("is_empty", ws.windows == 0)?;
        t.set("monitor", monitor(lua)?)?;
        Ok(t)
    }
    fn workspace_by_id(lua: &Lua, world: &World, id: i32) -> mlua::Result<Value> {
        match world.workspaces.iter().find(|w| w.id == id) {
            Some(ws) => Ok(Value::Table(workspace(lua, world, ws)?)),
            None => Ok(Value::Nil),
        }
    }
    fn window(lua: &Lua, world: &World, w: &WindowView) -> mlua::Result<Table> {
        let t = lua.create_table()?;
        let xy = |x: f64, y: f64| -> mlua::Result<Table> {
            let p = lua.create_table()?;
            p.set("x", x)?;
            p.set("y", y)?;
            Ok(p)
        };
        t.set("address", w.address.clone())?;
        t.set("mapped", true)?;
        t.set("hidden", false)?;
        t.set("visible", w.workspace == world.active || Some(w.workspace) == world.special)?;
        t.set("at", xy(w.at.0, w.at.1)?)?;
        t.set("size", xy(w.size.0, w.size.1)?)?;
        t.set("workspace", workspace_by_id(lua, world, w.workspace)?)?;
        t.set("floating", w.floating)?;
        t.set("monitor", monitor(lua)?)?;
        t.set("class", w.class.clone())?;
        t.set("title", w.title.clone())?;
        t.set("initial_class", w.class.clone())?;
        t.set("initial_title", w.title.clone())?;
        t.set("pid", w.pid)?;
        t.set("xwayland", false)?;
        t.set("pinned", false)?;
        t.set("fullscreen", if w.fullscreen { 2 } else { 0 })?;
        t.set("focus_history_id", w.focus_history_id)?;
        t.set("active", world.focused.as_deref() == Some(w.address.as_str()))?;
        t.set("tags", lua.create_table()?)?;
        if let Some(g) = &w.group {
            let group = lua.create_table()?;
            group.set("size", g.members.len())?;
            group.set("current_index", g.current_index)?;
            group.set("locked", g.locked)?;
            group.set("denied", false)?;
            let members = lua.create_table()?;
            for (i, address) in g.members.iter().enumerate() {
                let m = lua.create_table()?;
                m.set("address", address.clone())?;
                if let Some(v) = world.windows.iter().find(|v| v.address == *address) {
                    m.set("class", v.class.clone())?;
                    m.set("title", v.title.clone())?;
                }
                members.set(i + 1, m)?;
            }
            group.set("members", members)?;
            t.set("group", group)?;
        }
        Ok(t)
    }
    let query = |f: fn(&Lua, &World, MultiValue) -> mlua::Result<Value>| {
        let shared = shared.clone();
        lua.create_function(move |lua, args: MultiValue| {
            let world = shared.borrow().world.clone();
            f(lua, &world, args)
        })
    };
    hl.set(
        "get_active_window",
        query(|lua, world, _| match world.windows.iter().find(|w| world.focused.as_deref() == Some(w.address.as_str())) {
            Some(w) => Ok(Value::Table(window(lua, world, w)?)),
            None => Ok(Value::Nil),
        })?,
    )?;
    hl.set(
        "get_windows",
        query(|lua, world, _| {
            let list = lua.create_table()?;
            for (i, w) in world.windows.iter().enumerate() {
                list.set(i + 1, window(lua, world, w)?)?;
            }
            Ok(Value::Table(list))
        })?,
    )?;
    hl.set(
        "get_window",
        query(|lua, world, args| {
            let address = args.into_iter().next().and_then(|v| v.to_string().ok()).unwrap_or_default();
            match world.windows.iter().find(|w| w.address == address) {
                Some(w) => Ok(Value::Table(window(lua, world, w)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    hl.set("get_active_workspace", query(|lua, world, _| workspace_by_id(lua, world, world.active))?)?;
    hl.set(
        "get_active_special_workspace",
        query(|lua, world, _| match world.special {
            Some(id) => workspace_by_id(lua, world, id),
            None => Ok(Value::Nil),
        })?,
    )?;
    hl.set(
        "get_workspaces",
        query(|lua, world, _| {
            let list = lua.create_table()?;
            for (i, ws) in world.workspaces.iter().enumerate() {
                list.set(i + 1, workspace(lua, world, ws)?)?;
            }
            Ok(Value::Table(list))
        })?,
    )?;
    hl.set(
        "get_workspace",
        query(|lua, world, args| {
            let key = args.into_iter().next().unwrap_or(Value::Nil);
            let found = world.workspaces.iter().find(|ws| match &key {
                Value::Integer(n) => i64::from(ws.id) == *n,
                Value::Number(n) => f64::from(ws.id) == *n,
                Value::String(s) => s.to_str().is_ok_and(|s| *s == ws.name || s.parse::<i32>() == Ok(ws.id)),
                _ => false,
            });
            match found {
                Some(ws) => Ok(Value::Table(workspace(lua, world, ws)?)),
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    hl.set(
        "get_cursor_pos",
        query(|lua, world, _| {
            let p = lua.create_table()?;
            p.set("x", world.cursor.0)?;
            p.set("y", world.cursor.1)?;
            Ok(Value::Table(p))
        })?,
    )?;
    Ok(())
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
                        // hyprutils' defaults; "dampening" is Hyprland's old spelling.
                        mass: t.get::<Option<f64>>("mass")?.unwrap_or(1.0),
                        stiffness: t.get::<Option<f64>>("stiffness")?.unwrap_or(250.0),
                        damping: t.get::<Option<f64>>("damping")?.or(t.get::<Option<f64>>("dampening")?).unwrap_or(25.0),
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

    // hl.window_rule({ name, match = { ... }, effects... }): a handle with set_enabled.
    {
        let shared = shared.clone();
        hl.set(
            "window_rule",
            lua.create_function(move |lua, t: Table| {
                let mut warnings = Vec::new();
                let rule = crate::rules::WindowRule::parse(&t, |w| warnings.push(w.to_string()))?;
                let enabled = rule.enabled.clone();
                let mut s = shared.borrow_mut();
                for w in warnings {
                    s.warn_once(&w);
                }
                s.window_rules.push(rule);
                lua.create_userdata(Handle(Some(enabled)))
            })?,
        )?;
    }

    // hl.gesture({ fingers, direction, action })
    {
        let shared = shared.clone();
        hl.set(
            "gesture",
            lua.create_function(move |_, t: Table| {
                let g = Gesture {
                    fingers: t.get::<Option<u32>>("fingers")?.unwrap_or(3),
                    direction: opt_str(&t, "direction")?.unwrap_or_else(|| "horizontal".into()),
                    action: opt_str(&t, "action")?.unwrap_or_default(),
                };
                let mut s = shared.borrow_mut();
                if g.action != "workspace" {
                    s.warn_once(&format!("hl.gesture action {:?}", g.action));
                }
                s.gestures.push(g);
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
                    curve: opt_str(&t, "bezier")?.or(opt_str(&t, "spring")?.map(|s| format!("spring:{s}"))),
                    style: opt_str(&t, "style")?,
                };
                shared.borrow_mut().animations.insert(leaf, anim);
                Ok(())
            })?,
        )?;
    }

    install_queries(lua, &hl, shared)?;

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
    let workspace = lua.create_table()?;
    workspace.set(
        "toggle_special",
        lua.create_function(|lua, name: Option<String>| {
            lua.create_userdata(Dsp(Action::ToggleSpecial(name.unwrap_or_else(|| "special".into()))))
        })?,
    )?;
    dsp.set("workspace", workspace)?;
    let group = lua.create_table()?;
    group.set("toggle", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::ToggleGroup)))?)?;
    group.set("next", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::GroupCycle(1))))?)?;
    group.set("prev", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::GroupCycle(-1))))?)?;
    group.set(
        "move_window",
        lua.create_function(|lua, t: Option<Table>| {
            let forward = match t {
                Some(t) => t.get::<Option<bool>>("forward")?.unwrap_or(true),
                None => true,
            };
            lua.create_userdata(Dsp(Action::GroupMoveWindow { forward }))
        })?,
    )?;
    group.set(
        "lock",
        lua.create_function(|lua, t: Option<Table>| {
            let action = match &t {
                Some(t) => opt_str(t, "action")?,
                None => None,
            };
            let on = match action.as_deref() {
                Some("lock") | Some("set") => Some(true),
                Some("unlock") | Some("unset") => Some(false),
                _ => None,
            };
            lua.create_userdata(Dsp(Action::LockGroup(on)))
        })?,
    )?;
    dsp.set("group", group)?;
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
                if t.get::<Option<bool>>("group_aware")?.unwrap_or(false) {
                    Action::MoveWindowOrGroup(d)
                } else {
                    Action::MoveWindow(d)
                }
            } else {
                Action::Unsupported("hl.dsp.window.move with these options".into())
            };
            lua.create_userdata(Dsp(action))
        })?,
    )?;
    window.set("drag", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::MouseDrag)))?)?;
    window.set("resize", lua.create_function(|lua, _: MultiValue| lua.create_userdata(Dsp(Action::MouseResize)))?)?;
    // Unknown window dispatchers (pseudo, pin, ...).
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
        // A file per call: tests run in parallel, and two loading the same
        // source must not write one file while the other reads it.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = dir.join(format!("{}.lua", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
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
    fn colors() {
        let c = |s: &str| parse_color(&ConfigValue::Str(s.into())).unwrap();
        assert_eq!(c("rgb(00ff00)"), [0.0, 1.0, 0.0, 1.0]);
        assert_eq!(c("0xff0000ff"), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(parse_color(&ConfigValue::Int(0xee1a1a1a)).unwrap()[3], 0xee as f32 / 255.0);
        let half = c("rgba(ff000080)");
        assert!((half[0] - 0.502).abs() < 0.01 && (half[3] - 0.502).abs() < 0.01, "premultiplied");
    }

    #[test]
    fn default_config_loads() {
        let c = Config::load(Some(Path::new("/nonexistent/hyprland.lua")));
        assert!(!c.shared.borrow().binds.is_empty());
    }

    #[test]
    fn group_helpers_see_the_group() {
        // The owner's Cmd+h/l: next/previous tab while there is one that way, else focus.
        let c = load_str(include_str!("../../docs/exampleconfig.lua"));
        let run = |keys: &str, world: &World| {
            c.set_world(world.clone());
            let chord = crate::binds::parse_chord(keys).unwrap();
            c.run_bind(c.find_bind(chord.mods, chord.keysym).unwrap())
        };
        let group = |current| GroupView { current_index: current, locked: false, members: vec!["0x1".into(), "0x2".into()] };
        let mut world = World {
            active: 1,
            focused: Some("0x1".into()),
            windows: vec![WindowView { address: "0x1".into(), workspace: 1, group: Some(group(1)), ..Default::default() }],
            workspaces: vec![WorkspaceView { id: 1, name: "1".into(), windows: 2, ..Default::default() }],
            ..Default::default()
        };
        assert!(matches!(run("SUPER + l", &world).as_slice(), [Action::GroupCycle(1)]));
        assert!(matches!(run("SUPER + h", &world).as_slice(), [Action::Focus(Direction::Left)]));
        assert!(matches!(run("SUPER + SHIFT + l", &world).as_slice(), [Action::GroupMoveWindow { forward: true }]));
        assert!(matches!(run("SUPER + SHIFT + h", &world).as_slice(), [Action::MoveWindowOrGroup(Direction::Left)]));
        world.windows[0].group = Some(group(2));
        assert!(matches!(run("SUPER + h", &world).as_slice(), [Action::GroupCycle(-1)]));
        assert!(matches!(run("SUPER + w", &world).as_slice(), [Action::ToggleGroup]));
        world.windows[0].group = None;
        assert!(matches!(run("SUPER + l", &world).as_slice(), [Action::Focus(Direction::Right)]));
    }

    #[test]
    fn users_window_rules() {
        use crate::rules::{Facts, evaluate};
        let c = load_str(include_str!("../../docs/exampleconfig.lua"));
        let rules = c.window_rules();
        let facts = |class: &str, title: &str| Facts { class: class.into(), title: title.into(), ..Default::default() };
        assert_eq!(evaluate(&rules, &facts("file-roller", "Archive")).float, Some(true));
        assert_eq!(evaluate(&rules, &facts("org.gtk.Dialog", "Open File")).float, Some(true));
        assert_eq!(evaluate(&rules, &facts("vte-2.91-gtk4", "Terminal")).float, None);
        let run = evaluate(&rules, &facts("hyprland-run", ""));
        assert_eq!((run.float, run.move_to.as_deref()), (Some(true), Some("20 monitor_h-120")));
        // Every window gets the maximize suppression; the XWayland drag fix matches nothing here.
        let any = evaluate(&rules, &facts("", ""));
        assert_eq!(any.suppress, ["maximize"]);
        assert!(!any.no_focus);
    }

    #[test]
    fn scratchpad_bind_sees_the_window() {
        // The owner's Cmd+Shift+Minus: into special:magic, or back out of it.
        let c = load_str(include_str!("../../docs/exampleconfig.lua"));
        let chord = crate::binds::parse_chord("SUPER + SHIFT + MINUS").unwrap();
        let mut world = World {
            active: 2,
            focused: Some("0x10".into()),
            windows: vec![WindowView { address: "0x10".into(), workspace: 2, ..Default::default() }],
            workspaces: vec![
                WorkspaceView { id: 2, name: "2".into(), windows: 1, ..Default::default() },
                WorkspaceView { id: -98, name: "special:magic".into(), ..Default::default() },
            ],
            ..Default::default()
        };
        c.set_world(world.clone());
        let actions = c.run_bind(c.find_bind(chord.mods, chord.keysym).unwrap());
        assert!(matches!(
            actions.as_slice(),
            [Action::MoveToWorkspace { target: WorkspaceTarget::Special(n), follow: true }] if n == "magic"
        ));
        world.windows[0].workspace = -98;
        world.special = Some(-98);
        c.set_world(world);
        let actions = c.run_bind(c.find_bind(chord.mods, chord.keysym).unwrap());
        assert!(matches!(actions.as_slice(), [Action::MoveToWorkspace { target: WorkspaceTarget::Id(2), .. }]));
    }

    #[test]
    fn users_linux_config_loads() {
        // The project owner's Linux Hyprland config (docs/exampleconfig.lua).
        let c = load_str(include_str!("../../docs/exampleconfig.lua"));
        assert!(c.shared.borrow().binds.len() > 30);
        assert_eq!(c.settings.gaps_in, Gaps::all(1.0));
        assert_eq!(c.settings.border_size, 1);
        assert!(c.settings.curves.contains_key("easy"));
        assert_eq!(c.settings.active_border.colors.len(), 2);
        assert_eq!(c.settings.active_border.angle, 45.0);
        assert_eq!(c.settings.inactive_border.colors.len(), 1);
        let (shadow, blur) = (&c.settings.shadow, &c.settings.blur);
        assert!(shadow.enabled && blur.enabled);
        assert_eq!((shadow.range, shadow.power), (4.0, 3.0));
        assert!((shadow.color[3] - 0xee as f32 / 255.0).abs() < 1e-6);
        assert_eq!((blur.size, blur.passes), (3.0, 1));
        assert!((blur.vibrancy - 0.1696).abs() < 1e-9);
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
