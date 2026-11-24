//! Animations, as Hyprland does them: every animated value has where it
//! began, where it is and where it's going, and a timing from the config's
//! `hl.animation` tree (a leaf inherits from its parent unless set itself).
//!
//! A bezier animation lasts `speed` × 100 ms and follows the curve's y for
//! the elapsed fraction; a spring runs until it settles. The bezier baking and
//! lookup and the spring integration are ported from hyprutils
//! (src/animation/BezierCurve.cpp and Spring.cpp; Copyright (c) 2024, Hypr
//! Development, BSD 3-Clause, see tests/HYPRUTILS-LICENSE); the leaf tree, the
//! window styles (popin, gnomed, slide) and the workspace styles (slide,
//! slidevert, fade, slidefade) from Hyprland v0.57.2 (AnimationTree.cpp,
//! WindowAnimationController.cpp, WorkspaceAnimationController.cpp; BSD
//! 3-Clause, see tests/HYPRLAND-LICENSE).
//!
//! Each frame, `State::animate` points every visible window's animations at
//! their goals (the layout's rectangle, the focus colour) and advances them;
//! the renderer draws the current values. Input and layout always use the
//! goals.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    time::Instant,
};

use smithay::{desktop::Window, wayland::seat::WaylandFocus};

use crate::{
    config::{Curve as CurveConfig, Gradient, Settings},
    dwindle::Rect,
    render::Piece,
    state::State,
};

// ---- curves ----

const BAKED: usize = 255;

/// A cubic bezier from (0,0) to (1,1), baked into points for lookup.
pub struct Bezier {
    baked: [(f32, f32); BAKED],
}

impl Bezier {
    pub fn new(p1: (f64, f64), p2: (f64, f64)) -> Bezier {
        let pts = [(0.0f32, 0.0f32), (p1.0 as f32, p1.1 as f32), (p2.0 as f32, p2.1 as f32), (1.0, 1.0)];
        let at = |t: f32, k: fn(&(f32, f32)) -> f32| {
            let u = 1.0 - t;
            u * u * u * k(&pts[0]) + 3.0 * t * u * u * k(&pts[1]) + 3.0 * t * t * u * k(&pts[2]) + t * t * t * k(&pts[3])
        };
        let mut baked = [(0.0, 0.0); BAKED];
        // Baked from t = 1/255, not 0, as hyprutils does.
        for (i, p) in baked.iter_mut().enumerate() {
            let t = (i + 1) as f32 / BAKED as f32;
            *p = (at(t, |p| p.0), at(t, |p| p.1));
        }
        Bezier { baked }
    }

    /// The curve's y where its x is `x`.
    pub fn y_for(&self, x: f32) -> f32 {
        if x >= 1.0 {
            return 1.0;
        }
        if x <= 0.0 {
            return 0.0;
        }
        let mut index: i32 = 0;
        let mut below = true;
        let mut step = (BAKED as i32 + 1) / 2;
        while step > 0 {
            index += if below { step } else { -step };
            index = index.clamp(0, BAKED as i32 - 1);
            below = self.baked[index as usize].0 < x;
            step /= 2;
        }
        let lower = (index - (!below || index == BAKED as i32 - 1) as i32).clamp(0, BAKED as i32 - 2) as usize;
        let (lo, hi) = (self.baked[lower], self.baked[lower + 1]);
        let dx = hi.0 - lo.0;
        if dx <= 1e-6 {
            return lo.1;
        }
        let f = (x - lo.0) / dx;
        if !f.is_finite() {
            return lo.1;
        }
        lo.1 + (hi.1 - lo.1) * f
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spring {
    pub mass: f32,
    pub stiffness: f32,
    pub damping: f32,
}

/// hyprutils' default spring.
const DEFAULT_SPRING: Spring = Spring { mass: 1.0, stiffness: 250.0, damping: 25.0 };
const SPRING_EPSILON: f32 = 0.001;

/// Advances a damped spring pulling `value` towards 1 by `dt` seconds.
fn advance_spring(value: &mut f32, velocity: &mut f32, s: &Spring, dt: f32) {
    if dt <= 0.0 {
        return;
    }
    let mass = s.mass.max(0.0001);
    let stiffness = s.stiffness.max(0.0001);
    let damping = s.damping.max(0.0);
    let x = *value - 1.0;
    let omega0 = (stiffness / mass).sqrt();
    let gamma = damping / (2.0 * mass);
    if gamma < omega0 {
        let omegad = (omega0 * omega0 - gamma * gamma).sqrt();
        let (e, sin, cos) = ((-gamma * dt).exp(), (omegad * dt).sin(), (omegad * dt).cos());
        let nx = e * (x * cos + ((*velocity + gamma * x) / omegad) * sin);
        let nv = e * (*velocity * cos - ((gamma * *velocity + omega0 * omega0 * x) / omegad) * sin);
        *value = 1.0 + nx;
        *velocity = nv;
        return;
    }
    if (gamma - omega0).abs() <= omega0.max(1.0) * 0.0001 {
        let e = (-gamma * dt).exp();
        let b = *velocity + gamma * x;
        *value = 1.0 + e * (x + b * dt);
        *velocity = e * (*velocity - gamma * b * dt);
        return;
    }
    let root = (gamma * gamma - omega0 * omega0).sqrt();
    let (r1, r2) = (-gamma + root, -gamma - root);
    let a = (*velocity - r2 * x) / (r1 - r2);
    let b = x - a;
    let (e1, e2) = ((r1 * dt).exp(), (r2 * dt).exp());
    *value = 1.0 + a * e1 + b * e2;
    *velocity = a * r1 * e1 + b * r2 * e2;
}

#[derive(Clone)]
pub enum Curve {
    Bezier(Rc<Bezier>),
    Spring(Spring),
}

/// How one leaf animates.
#[derive(Clone)]
pub struct Timing {
    pub enabled: bool,
    /// Seconds (bezier curves).
    pub duration: f32,
    pub curve: Curve,
    pub style: String,
}

/// Hyprland's animation tree: (leaf, parent).
const TREE: &[(&str, &str)] = &[
    ("global", ""),
    ("windows", "global"),
    ("layers", "global"),
    ("fade", "global"),
    ("border", "global"),
    ("borderangle", "global"),
    ("shadowangle", "global"),
    ("glowangle", "global"),
    ("workspaces", "global"),
    ("zoomFactor", "global"),
    ("monitorAdded", "global"),
    ("layersIn", "layers"),
    ("layersOut", "layers"),
    ("windowsIn", "windows"),
    ("windowsOut", "windows"),
    ("windowsMove", "windows"),
    ("fadeIn", "fade"),
    ("fadeOut", "fade"),
    ("fadeSwitch", "fade"),
    ("fadeShadow", "fade"),
    ("fadeGlow", "fade"),
    ("fadeDim", "fade"),
    ("fadeLayers", "fade"),
    ("fadeLayersIn", "fadeLayers"),
    ("fadeLayersOut", "fadeLayers"),
    ("fadePopups", "fade"),
    ("fadePopupsIn", "fadePopups"),
    ("fadePopupsOut", "fadePopups"),
    ("fadeDpms", "fade"),
    ("workspacesIn", "workspaces"),
    ("workspacesOut", "workspaces"),
    ("specialWorkspace", "workspaces"),
    ("specialWorkspaceIn", "specialWorkspace"),
    ("specialWorkspaceOut", "specialWorkspace"),
];

/// Every leaf's timing, resolved from the config.
pub struct Timings {
    leaves: HashMap<&'static str, Timing>,
}

impl Timings {
    pub fn new(settings: &Settings) -> Timings {
        let mut beziers: HashMap<String, Rc<Bezier>> = HashMap::new();
        // hyprutils' built-in "default", and Hyprland's "linear".
        beziers.insert("default".into(), Rc::new(Bezier::new((0.0, 0.75), (0.15, 1.0))));
        beziers.insert("linear".into(), Rc::new(Bezier::new((0.0, 0.0), (1.0, 1.0))));
        let mut springs: HashMap<String, Spring> = HashMap::new();
        springs.insert("default".into(), DEFAULT_SPRING);
        for (name, curve) in &settings.curves {
            match curve {
                CurveConfig::Bezier([p1, p2]) => {
                    beziers.insert(name.clone(), Rc::new(Bezier::new(*p1, *p2)));
                }
                CurveConfig::Spring { mass, stiffness, damping } => {
                    let s = Spring { mass: *mass as f32, stiffness: *stiffness as f32, damping: *damping as f32 };
                    springs.insert(name.clone(), s);
                }
            }
        }
        let curve = |name: &str| match name.strip_prefix("spring:") {
            Some(s) => Curve::Spring(springs.get(s).copied().unwrap_or(DEFAULT_SPRING)),
            None => Curve::Bezier(beziers.get(name).unwrap_or(&beziers["default"]).clone()),
        };
        // A leaf's own setting, else its parent's, up to global.
        let own = |leaf: &str| -> Option<Timing> {
            if let Some(a) = settings.animations.get(leaf) {
                return Some(Timing {
                    enabled: a.enabled,
                    duration: (a.speed.max(0.0) * 0.1) as f32,
                    curve: curve(a.curve.as_deref().unwrap_or("default")),
                    style: a.style.clone().unwrap_or_default(),
                });
            }
            let builtin = |enabled, speed: f32, name: &str| Timing {
                enabled,
                duration: speed * 0.1,
                curve: curve(name),
                style: String::new(),
            };
            match leaf {
                "global" => Some(builtin(true, 8.0, "default")),
                "borderangle" | "shadowangle" | "glowangle" => Some(builtin(false, 1.0, "default")),
                _ => None,
            }
        };
        let mut leaves = HashMap::new();
        for (leaf, _) in TREE {
            let mut node = *leaf;
            let timing = loop {
                if let Some(t) = own(node) {
                    break t;
                }
                node = TREE.iter().find(|(n, _)| *n == node).map(|(_, p)| *p).unwrap_or("");
                if node.is_empty() {
                    break own("global").unwrap();
                }
            };
            let timing = Timing { enabled: timing.enabled && settings.animations_enabled, ..timing };
            leaves.insert(*leaf, timing);
        }
        Timings { leaves }
    }

    pub fn get(&self, leaf: &str) -> &Timing {
        &self.leaves[leaf]
    }
}

// ---- animated values ----

pub trait Lerp: Clone + PartialEq {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self;
}

impl Lerp for f64 {
    fn lerp(a: &f64, b: &f64, t: f32) -> f64 {
        a + (b - a) * t as f64
    }
}

impl Lerp for [f64; 2] {
    fn lerp(a: &Self, b: &Self, t: f32) -> Self {
        [f64::lerp(&a[0], &b[0], t), f64::lerp(&a[1], &b[1], t)]
    }
}

impl Lerp for Rect {
    fn lerp(a: &Rect, b: &Rect, t: f32) -> Rect {
        let l = |x: f64, y: f64| f64::lerp(&x, &y, t);
        Rect { x: l(a.x, b.x), y: l(a.y, b.y), w: l(a.w, b.w), h: l(a.h, b.h) }
    }
}

/// A border gradient as animated: colours mixed in OkLab, as Hyprland does.
impl Lerp for Gradient {
    fn lerp(a: &Gradient, b: &Gradient, t: f32) -> Gradient {
        let n = a.colors.len().max(b.colors.len()).clamp(1, 8);
        let colors = (0..n)
            .map(|i| {
                let pos = if n == 1 { 0.0 } else { i as f32 / (n - 1) as f32 };
                mix_oklab(sample(a, pos), sample(b, pos), t)
            })
            .collect();
        Gradient { colors, angle: a.angle + (b.angle - a.angle) * t }
    }
}

/// A gradient's colour at `pos` (0..1), its stops evenly spaced.
fn sample(g: &Gradient, pos: f32) -> [f32; 4] {
    match g.colors.len() {
        0 => [0.0; 4],
        1 => g.colors[0],
        n => {
            let p = pos.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = (p.floor() as usize).min(n - 2);
            let f = p - i as f32;
            let (c0, c1) = (g.colors[i], g.colors[i + 1]);
            std::array::from_fn(|k| c0[k] + (c1[k] - c0[k]) * f)
        }
    }
}

/// Mixes two premultiplied sRGB colours in OkLab.
fn mix_oklab(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let la = to_oklab(a);
    let lb = to_oklab(b);
    let l: [f32; 3] = std::array::from_fn(|k| la[k] + (lb[k] - la[k]) * t);
    let alpha = a[3] + (b[3] - a[3]) * t;
    let rgb = from_oklab(l);
    [rgb[0] * alpha, rgb[1] * alpha, rgb[2] * alpha, alpha]
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.0031308 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

#[allow(clippy::excessive_precision)] // Ottosson's published OkLab matrices
fn to_oklab(c: [f32; 4]) -> [f32; 3] {
    let a = c[3];
    let un = |v: f32| if a > 0.0 { to_linear((v / a).clamp(0.0, 1.0)) } else { 0.0 };
    let (r, g, b) = (un(c[0]), un(c[1]), un(c[2]));
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

#[allow(clippy::excessive_precision)]
fn from_oklab(c: [f32; 3]) -> [f32; 3] {
    let l = (c[0] + 0.3963377774 * c[1] + 0.2158037573 * c[2]).powi(3);
    let m = (c[0] - 0.1055613458 * c[1] - 0.0638541728 * c[2]).powi(3);
    let s = (c[0] - 0.0894841775 * c[1] - 1.2914855480 * c[2]).powi(3);
    [
        to_srgb(4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s),
        to_srgb(-1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s),
        to_srgb(-0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s),
    ]
}

/// A value animating towards a goal.
pub struct Animated<T> {
    begun: T,
    value: T,
    goal: T,
    start: Instant,
    curve: Option<(Curve, f32)>,
    spring_value: f32,
    spring_velocity: f32,
    last_step: Instant,
}

impl<T: Lerp> Animated<T> {
    pub fn new(v: T) -> Animated<T> {
        let now = Instant::now();
        Animated {
            begun: v.clone(),
            value: v.clone(),
            goal: v,
            start: now,
            curve: None,
            spring_value: 1.0,
            spring_velocity: 0.0,
            last_step: now,
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn goal(&self) -> &T {
        &self.goal
    }

    pub fn active(&self) -> bool {
        self.curve.is_some()
    }

    /// Jump to a value, ending any animation.
    pub fn warp(&mut self, v: T) {
        self.begun = v.clone();
        self.value = v.clone();
        self.goal = v;
        self.curve = None;
        self.spring_velocity = 0.0;
    }

    /// Start (or redirect) an animation from the current value to `goal`.
    pub fn set(&mut self, goal: T, timing: &Timing, now: Instant) {
        if goal == self.goal {
            return;
        }
        if !timing.enabled {
            self.warp(goal);
            return;
        }
        let was_spring = matches!(self.curve, Some((Curve::Spring(_), _)));
        self.begun = self.value.clone();
        self.goal = goal;
        self.start = now;
        if let Curve::Spring(_) = timing.curve {
            // A redirected spring keeps its velocity (hyprutils).
            self.spring_value = 0.0;
            if !was_spring {
                self.spring_velocity = 0.0;
            }
            self.last_step = now;
        }
        self.curve = Some((timing.curve.clone(), timing.duration));
    }

    /// Set the start and the goal at once (an opening window's popin).
    pub fn animate_from(&mut self, from: T, goal: T, timing: &Timing, now: Instant) {
        self.warp(from);
        self.set(goal, timing, now);
    }

    /// Advance to `now`. True if it was animating.
    pub fn tick(&mut self, now: Instant) -> bool {
        let Some((curve, duration)) = &self.curve else { return false };
        let (t, done) = match curve {
            Curve::Bezier(b) => {
                let spent = if *duration <= 0.0 {
                    1.0
                } else {
                    now.saturating_duration_since(self.start).as_secs_f32() / duration
                };
                if spent >= 1.0 { (1.0, true) } else { (b.y_for(spent), false) }
            }
            Curve::Spring(s) => {
                let dt = now.saturating_duration_since(self.last_step).as_secs_f32();
                self.last_step = now;
                advance_spring(&mut self.spring_value, &mut self.spring_velocity, s, dt);
                let done = (1.0 - self.spring_value).abs() <= SPRING_EPSILON
                    && self.spring_velocity.abs() <= SPRING_EPSILON;
                (self.spring_value, done)
            }
        };
        if done {
            self.value = self.goal.clone();
            self.curve = None;
            self.spring_value = 1.0;
            self.spring_velocity = 0.0;
        } else {
            self.value = T::lerp(&self.begun, &self.goal, t);
        }
        true
    }
}

// ---- windows ----

/// A window's animations, in its user data once it has shown a buffer.
pub struct WindowAnim {
    /// The visible rectangle (logical).
    pub rect: Animated<Rect>,
    pub alpha: Animated<f64>,
    pub border: Animated<Gradient>,
    /// The `State::anim_frame` this window was last animated in: a window
    /// that wasn't on screen jumps to its place instead of sliding there.
    last_frame: u64,
}

impl WindowAnim {
    fn tick(&mut self, now: Instant) -> bool {
        // Not short-circuited: all three advance.
        self.rect.tick(now) | self.alpha.tick(now) | self.border.tick(now)
    }
}

type AnimSlot = RefCell<Option<WindowAnim>>;

/// Set by `manage`: the window's first buffer animates in.
pub struct Fresh(Cell<bool>);

pub fn mark_fresh(window: &Window) {
    window.user_data().insert_if_missing(|| Fresh(Cell::new(true)));
}

/// Runs `f` with a window's animations, if it has any yet.
pub fn with_anim<R>(window: &Window, f: impl FnOnce(&WindowAnim) -> R) -> Option<R> {
    window.user_data().get::<AnimSlot>().and_then(|slot| slot.borrow().as_ref().map(f))
}

/// The start (opening) or the end (closing) of a window style: Hyprland's
/// `popin N%` (the default, from N% of the size, centred), `gnomed` (from a
/// horizontal line through the middle) and `slide [top|bottom|left|right]`
/// (from the nearest output edge, or the given one).
fn styled(style: &str, goal: Rect, output: Rect) -> Rect {
    let style = style.to_lowercase();
    let mut words = style.split_whitespace();
    match words.next().unwrap_or("") {
        "slide" => {
            let (x, y, w, h) = (goal.x, goal.y, goal.w, goal.h);
            let edge = match words.next() {
                Some(e) => e.to_string(),
                None => {
                    let (mx, my) = (x + w / 2.0, y + h / 2.0);
                    let d = [my - output.y, output.x + output.w - mx, output.y + output.h - my, mx - output.x];
                    let min = d.iter().copied().fold(f64::INFINITY, f64::min);
                    if min == d[2] {
                        "bottom"
                    } else if min == d[3] {
                        "left"
                    } else if min == d[1] {
                        "right"
                    } else {
                        "top"
                    }
                    .into()
                }
            };
            match edge.as_str() {
                "bottom" => Rect { y: output.y + output.h, ..goal },
                "left" => Rect { x: x - w, ..goal },
                "right" => Rect { x: x + w, ..goal },
                _ => Rect { y: output.y - h, ..goal },
            }
        }
        "gnomed" | "gnome" => Rect { x: goal.x, y: goal.y + goal.h / 2.0, w: goal.w, h: 0.0 },
        _ => {
            // popin: the percentage is the last word ("popin 87%"); none is 0.
            let perc = style
                .rsplit(' ')
                .next()
                .filter(|w| w.ends_with('%') && !style.starts_with('%'))
                .and_then(|w| w.trim_end_matches('%').parse::<f64>().ok())
                .unwrap_or(0.0)
                / 100.0;
            let w = (goal.w * perc).clamp(5.0_f64.min(goal.w), goal.w);
            let h = (goal.h * perc).clamp(5.0_f64.min(goal.h), goal.h);
            Rect { x: goal.x + (goal.w - w) / 2.0, y: goal.y + (goal.h - h) / 2.0, w, h }
        }
    }
}

/// A window that closed, drawn from its last textures while it animates out.
pub struct Closing {
    pub pieces: Vec<Piece>,
    pub rect: Animated<Rect>,
    pub alpha: Animated<f64>,
    pub border: Gradient,
    /// Where the pieces were laid out (the window's goal when it closed).
    pub anchor: Rect,
    pub decorated: bool,
    pub clipped: bool,
}

// ---- workspaces ----

/// A workspace switch in progress: the workspace left slides or fades out,
/// the new one in.
pub struct Transition {
    pub from: i32,
    pub out_alpha: Animated<f64>,
    pub out_offset: Animated<[f64; 2]>,
    pub in_alpha: Animated<f64>,
    pub in_offset: Animated<[f64; 2]>,
}

impl Transition {
    fn tick(&mut self, now: Instant) -> bool {
        self.out_alpha.tick(now) | self.out_offset.tick(now) | self.in_alpha.tick(now) | self.in_offset.tick(now)
    }

    pub fn active(&self) -> bool {
        self.out_alpha.active() || self.out_offset.active() || self.in_alpha.active() || self.in_offset.active()
    }
}

/// One side of a workspace switch, as Hyprland's `startAnimation`: returns
/// (alpha from, alpha to, offset from, offset to).
fn workspace_side(style: &str, incoming: bool, mut left: bool, output: (f64, f64), gap: f64) -> (f64, f64, [f64; 2], [f64; 2]) {
    let mut vert = style.starts_with("slidevert") || style.starts_with("slidefadevert");
    let words: Vec<&str> = style.split_whitespace().collect();
    if let Some(dir) = words.get(1) {
        match *dir {
            "top" => (left, vert) = (false, true),
            "bottom" => (left, vert) = (true, true),
            "left" => (left, vert) = (false, false),
            "right" => (left, vert) = (true, false),
            _ => {}
        }
    }
    let perc = words
        .last()
        .filter(|w| w.ends_with('%'))
        .and_then(|w| w.trim_end_matches('%').parse::<f64>().ok())
        .unwrap_or(100.0)
        / 100.0;
    let along = |d: f64| if vert { [0.0, d] } else { [d, 0.0] };
    let size = if vert { output.1 } else { output.0 };
    let sign = if left { 1.0 } else { -1.0 };
    let zero = [0.0, 0.0];
    if style.starts_with("slidefade") {
        let d = size * perc;
        if incoming { (0.0, 1.0, along(sign * d), zero) } else { (1.0, 0.0, zero, along(-sign * d)) }
    } else if style == "fade" {
        if incoming { (0.0, 1.0, zero, zero) } else { (1.0, 0.0, zero, zero) }
    } else {
        let d = (size + gap) * perc;
        if incoming { (1.0, 1.0, along(sign * d), zero) } else { (1.0, 1.0, zero, along(-sign * d)) }
    }
}

// ---- the per-frame step ----

impl State {
    /// The rectangle a window shows in at rest: the client's window
    /// geometry where the layout put it, cut to its tile (or the output when
    /// fullscreen).
    pub fn goal_rect(&self, window: &Window) -> Option<Rect> {
        let loc = self.space.element_location(window)?;
        let full = smithay::utils::Rectangle::new(loc, window.geometry().size);
        let r = crate::wm::clip_of(window).and_then(|c| full.intersection(c)).unwrap_or(full);
        let (dx, dy) = self.drag_offset(window).unwrap_or_default();
        Some(Rect { x: r.loc.x as f64 + dx, y: r.loc.y as f64 + dy, w: r.size.w as f64, h: r.size.h as f64 })
    }

    fn output_rect(&self) -> Rect {
        Rect { x: 0.0, y: 0.0, w: self.output_size.w as f64, h: self.output_size.h as f64 }
    }

    /// Points every visible window's animations at their goals and advances
    /// everything to `now`. True while anything moved (draw again).
    pub fn animate(&mut self, now: Instant) -> bool {
        self.anim_frame += 1;
        let frame = self.anim_frame;
        let mut busy = false;
        let focused = self.focused_window();
        let output = self.output_rect();

        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            let Some(goal) = self.goal_rect(&window) else { continue };
            let s = &self.config.settings;
            let border =
                if focused.as_ref() == Some(&window) { s.active_border.clone() } else { s.inactive_border.clone() };
            let has_buffer = window.wl_surface().is_some_and(|surface| {
                smithay::wayland::compositor::with_states(&surface, |states| {
                    states
                        .data_map
                        .get::<smithay::backend::renderer::utils::RendererSurfaceStateUserData>()
                        .is_some_and(|d| d.lock().unwrap().view().is_some())
                })
            });
            let floating = self.is_floating_here(&window);
            let dragging = self.dragging(&window);
            let t = &self.timings;
            window.user_data().insert_if_missing(|| AnimSlot::new(None));
            let mut slot = window.user_data().get::<AnimSlot>().unwrap().borrow_mut();
            let anim = match slot.as_mut() {
                None => {
                    // Nothing to show before the first buffer.
                    if !has_buffer {
                        continue;
                    }
                    let mut a = WindowAnim {
                        rect: Animated::new(goal),
                        alpha: Animated::new(1.0),
                        border: Animated::new(border),
                        last_frame: frame,
                    };
                    let fresh = window.user_data().get::<Fresh>().is_some_and(|f| f.0.replace(false));
                    if fresh {
                        let timing = t.get("windowsIn");
                        a.rect.animate_from(styled(&timing.style, goal, output), goal, timing, now);
                        a.alpha.animate_from(0.0, 1.0, t.get("fadeIn"), now);
                    }
                    slot.insert(a)
                }
                Some(a) if a.last_frame + 1 != frame => {
                    // Back on screen (a workspace switch): no slide from where it was.
                    a.rect.warp(goal);
                    a.alpha.warp(1.0);
                    a.border.warp(border);
                    a
                }
                Some(a) => {
                    let current = *a.rect.goal();
                    if dragging || (floating && !a.rect.active() && (current.x, current.y) == (goal.x, goal.y)) {
                        // A floating client resizing itself: follow it.
                        a.rect.warp(goal);
                    } else {
                        a.rect.set(goal, t.get("windowsMove"), now);
                    }
                    a.border.set(border, t.get("border"), now);
                    a
                }
            };
            anim.last_frame = frame;
            busy |= anim.tick(now);
        }

        // The workspace being left: its windows keep animating while they slide out.
        if let Some(tr) = &mut self.wm.transition {
            busy |= tr.tick(now);
            let leaving = self.wm.workspaces.get(&tr.from).map(|ws| ws.windows()).unwrap_or_default();
            for w in leaving {
                if let Some(slot) = w.user_data().get::<AnimSlot>() {
                    if let Some(a) = slot.borrow_mut().as_mut() {
                        a.last_frame = frame;
                        busy |= a.tick(now);
                    }
                }
            }
            // A swipe holds the transition still while the fingers are down.
            if !tr.active() && !self.swipe.active() {
                self.wm.transition = None;
            }
        }

        for c in &mut self.closing {
            busy |= c.rect.tick(now) | c.alpha.tick(now);
        }
        self.closing.retain(|c| c.rect.active() || c.alpha.active());
        busy
    }

    /// A window is going away: keep its last frame and animate it out.
    pub fn animate_close(&mut self, window: &Window) {
        let Some(slot) = window.user_data().get::<AnimSlot>() else { return };
        let Some(anim) = slot.borrow_mut().take() else { return };
        let Some(surface) = window.wl_surface() else { return };
        let anchor = *anim.rect.goal();
        let origin = (anchor.x - window.geometry().loc.x as f64, anchor.y - window.geometry().loc.y as f64);
        let pieces = crate::render::snapshot(&surface, origin.into());
        if pieces.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut rect = anim.rect;
        let mut alpha = anim.alpha;
        let timing = self.timings.get("windowsOut");
        rect.set(styled(&timing.style, anchor, self.output_rect()), timing, now);
        alpha.set(0.0, self.timings.get("fadeOut"), now);
        if !rect.active() && !alpha.active() {
            return;
        }
        self.closing.push(Closing {
            pieces,
            rect,
            alpha,
            border: anim.border.value().clone(),
            anchor,
            decorated: !self.is_fullscreen(window),
            clipped: crate::wm::clip_of(window).is_some(),
        });
        self.needs_redraw = true;
    }

    /// Start the slide (or fade) from workspace `from` to the active one.
    pub fn animate_workspace_switch(&mut self, from: i32) {
        let to = self.wm.active;
        let left = to > from;
        let now = Instant::now();
        let output = (self.output_size.w as f64, self.output_size.h as f64);
        let gap = self.config.settings.workspace_gap;
        let side = |leaf: &str, incoming: bool| {
            let timing = self.timings.get(leaf);
            let (a0, a1, o0, o1) = workspace_side(&timing.style.to_lowercase(), incoming, left, output, gap);
            let mut alpha = Animated::new(a0);
            alpha.set(a1, timing, now);
            let mut offset = Animated::new(o0);
            offset.set(o1, timing, now);
            (alpha, offset)
        };
        let (out_alpha, out_offset) = side("workspacesOut", false);
        let (in_alpha, in_offset) = side("workspacesIn", true);
        let tr = Transition { from, out_alpha, out_offset, in_alpha, in_offset };
        self.wm.transition = tr.active().then_some(tr);
        self.needs_redraw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bezier_endpoints_and_linear() {
        let linear = Bezier::new((0.0, 0.0), (1.0, 1.0));
        assert_eq!(linear.y_for(0.0), 0.0);
        assert_eq!(linear.y_for(1.0), 1.0);
        assert!((linear.y_for(0.5) - 0.5).abs() < 0.01);
        // An ease-out is ahead of linear halfway through.
        let ease = Bezier::new((0.23, 1.0), (0.32, 1.0));
        assert!(ease.y_for(0.5) > 0.9);
    }

    #[test]
    fn spring_settles_at_one() {
        let (mut v, mut vel) = (0.0f32, 0.0f32);
        for _ in 0..300 {
            advance_spring(&mut v, &mut vel, &DEFAULT_SPRING, 1.0 / 60.0);
        }
        assert!((v - 1.0).abs() < 0.001 && vel.abs() < 0.01);
    }

    #[test]
    fn popin_style() {
        let goal = Rect::new(100.0, 100.0, 200.0, 100.0);
        let out = Rect::new(0.0, 0.0, 1000.0, 1000.0);
        let r = styled("popin 80%", goal, out);
        assert_eq!((r.w, r.h), (160.0, 80.0));
        assert_eq!((r.x, r.y), (120.0, 110.0));
        let g = styled("gnomed", goal, out);
        assert_eq!((g.h, g.y), (0.0, 150.0));
        let s = styled("slide left", goal, out);
        assert_eq!(s.x, -100.0);
    }

    #[test]
    fn workspace_slide_directions() {
        // Going right (to a higher workspace): the new one comes in from the right.
        let (_, _, from, to) = workspace_side("slide", true, true, (1000.0, 800.0), 0.0);
        assert_eq!((from, to), ([1000.0, 0.0], [0.0, 0.0]));
        let (_, _, from, to) = workspace_side("slide", false, true, (1000.0, 800.0), 0.0);
        assert_eq!((from, to), ([0.0, 0.0], [-1000.0, 0.0]));
        let (a0, a1, _, _) = workspace_side("fade", true, true, (1000.0, 800.0), 0.0);
        assert_eq!((a0, a1), (0.0, 1.0));
    }

    #[test]
    fn leaves_inherit() {
        let mut settings = Settings::from_flat(&Default::default());
        settings.animations.insert(
            "windows".into(),
            crate::config::AnimationConfig { enabled: true, speed: 3.0, curve: Some("linear".into()), style: None },
        );
        let t = Timings::new(&settings);
        assert!((t.get("windowsIn").duration - 0.3).abs() < 1e-6);
        assert!((t.get("fadeIn").duration - 0.8).abs() < 1e-6);
        assert!(!t.get("borderangle").enabled);
    }
}
