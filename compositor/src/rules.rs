//! Window rules: `hl.window_rule({ name, match = { ... }, <effects> })`, as
//! Hyprland 0.55+'s Lua rules.
//!
//! A rule applies to a window when every one of its `match` properties does.
//! String properties are regexes that must match the whole value (RE2's
//! `FullMatch`); a `negative:` prefix inverts one. Later rules override
//! earlier ones where they set the same effect.
//!
//! Matched: `class`, `title`, `initial_class`, `initial_title`, `float`,
//! `fullscreen`, `focus`, `modal`, `workspace` (a number or name, compared as
//! text), `xwayland` and `pin` (always false here). Any other property makes
//! the rule never match (and warns once).
//!
//! Effects when a window opens: `float`, `tile`, `size`, `move`, `center`,
//! `workspace` ("3", "special:magic", with " silent" to stay put),
//! `fullscreen`, `no_initial_focus`. While it's open: `no_focus`,
//! `suppress_event` ("fullscreen" is the request clients can make here),
//! `rounding`, `border_size` (drawn; the layout keeps the configured
//! border), `opacity` ("active [inactive]"), `no_shadow`, `no_blur`,
//! `no_anim`, `decorate`. Others warn once and do nothing.
//!
//! `move`/`size` are two expressions, "x y": numbers, `N%` (of the monitor's
//! width or height), `monitor_w`, `monitor_h`, `window_w`, `window_h`,
//! `cursor_x`, `cursor_y`, `+ - * /` and parentheses. A leading `onscreen` is
//! accepted and ignored.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use mlua::{Table, Value};
use regex::Regex;
use smithay::desktop::Window;

use crate::state::State;

enum Prop {
    Class,
    Title,
    InitialClass,
    InitialTitle,
    Float,
    Fullscreen,
    Focus,
    Modal,
    Workspace,
    Xwayland,
    Pin,
}

enum Matcher {
    Regex { re: Regex, negative: bool },
    Bool(bool),
}

pub struct WindowRule {
    pub name: String,
    pub enabled: Rc<Cell<bool>>,
    matchers: Vec<(Prop, Matcher)>,
    /// A match property we don't know: never matches.
    unmatchable: bool,
    effects: Effects,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effects {
    /// Some(true): float, Some(false): tile.
    pub float: Option<bool>,
    pub fullscreen: bool,
    pub size: Option<String>,
    pub move_to: Option<String>,
    pub center: bool,
    pub workspace: Option<String>,
    pub no_initial_focus: bool,
    pub no_focus: bool,
    pub suppress: Vec<String>,
    pub rounding: Option<f64>,
    pub border_size: Option<f64>,
    /// Active, inactive.
    pub opacity: Option<(f64, f64)>,
    pub no_shadow: bool,
    pub no_blur: bool,
    pub no_anim: bool,
    pub decorate: Option<bool>,
}

impl Effects {
    /// `other` (a later rule) over `self`.
    fn merge(&mut self, other: &Effects) {
        macro_rules! over {
            ($($f:ident),*) => { $( if other.$f.is_some() { self.$f = other.$f.clone(); } )* };
        }
        macro_rules! flag {
            ($($f:ident),*) => { $( self.$f |= other.$f; )* };
        }
        over!(float, size, move_to, workspace, rounding, border_size, opacity, decorate);
        flag!(fullscreen, center, no_initial_focus, no_focus, no_shadow, no_blur, no_anim);
        self.suppress.extend(other.suppress.iter().cloned());
    }
}

/// What a rule's `match` looks at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Facts {
    pub class: String,
    pub title: String,
    pub floating: bool,
    pub fullscreen: bool,
    pub focused: bool,
    pub modal: bool,
    pub workspace: String,
}

impl WindowRule {
    /// From `hl.window_rule`'s table. Problems are logged, and unknown
    /// effects passed to `warn` (once each).
    pub fn parse(t: &Table, mut warn: impl FnMut(&str)) -> mlua::Result<WindowRule> {
        let name: String = t.get::<Option<String>>("name")?.unwrap_or_default();
        let enabled = Rc::new(Cell::new(t.get::<Option<bool>>("enabled")?.unwrap_or(true)));
        let mut matchers = Vec::new();
        let mut unmatchable = false;
        if let Some(m) = t.get::<Option<Table>>("match")? {
            for pair in m.pairs::<String, Value>() {
                let (key, value) = pair?;
                let prop = match key.as_str() {
                    "class" => Prop::Class,
                    "title" => Prop::Title,
                    "initial_class" => Prop::InitialClass,
                    "initial_title" => Prop::InitialTitle,
                    "float" => Prop::Float,
                    "fullscreen" => Prop::Fullscreen,
                    "focus" => Prop::Focus,
                    "modal" => Prop::Modal,
                    "workspace" => Prop::Workspace,
                    "xwayland" => Prop::Xwayland,
                    "pin" => Prop::Pin,
                    other => {
                        warn(&format!("hl.window_rule match {other:?}"));
                        unmatchable = true;
                        continue;
                    }
                };
                let matcher = match value {
                    Value::Boolean(b) => Some(Matcher::Bool(b)),
                    Value::Integer(n) => regex_matcher(&regex::escape(&n.to_string()), false),
                    Value::String(s) => {
                        let s = s.to_str()?.to_string();
                        let (pattern, negative) = match s.strip_prefix("negative:") {
                            Some(p) => (p.to_string(), true),
                            None => (s, false),
                        };
                        regex_matcher(&pattern, negative)
                    }
                    _ => {
                        tracing::warn!("config: window rule {name:?}: match {key:?} isn't a string or boolean");
                        unmatchable = true;
                        continue;
                    }
                };
                match matcher {
                    Some(m) => matchers.push((prop, m)),
                    None => {
                        tracing::warn!("config: window rule {name:?}: bad regex for {key:?}");
                        unmatchable = true;
                    }
                }
            }
        }
        let mut effects = Effects::default();
        for pair in t.pairs::<String, Value>() {
            let (key, value) = pair?;
            let truthy = !matches!(value, Value::Boolean(false) | Value::Nil);
            let text = match &value {
                Value::String(s) => s.to_str().map(|s| s.to_string()).unwrap_or_default(),
                Value::Integer(n) => n.to_string(),
                Value::Number(n) => n.to_string(),
                Value::Boolean(b) => (*b as u8).to_string(),
                _ => String::new(),
            };
            let number = text.trim().parse::<f64>().ok();
            match key.as_str() {
                "name" | "match" | "enabled" => {}
                "float" if truthy => effects.float = Some(true),
                "tile" if truthy => effects.float = Some(false),
                "fullscreen" => effects.fullscreen = truthy,
                "maximize" => {} // no maximized state here
                "size" => effects.size = Some(text),
                "move" => effects.move_to = Some(text),
                "center" => effects.center = truthy,
                "workspace" => effects.workspace = Some(text),
                "no_initial_focus" => effects.no_initial_focus = truthy,
                "no_focus" => effects.no_focus = truthy,
                "suppress_event" => effects.suppress = text.split_whitespace().map(str::to_string).collect(),
                "rounding" => effects.rounding = number,
                "border_size" => effects.border_size = number,
                "opacity" => {
                    let v: Vec<f64> = text.split_whitespace().filter_map(|p| p.parse().ok()).collect();
                    effects.opacity = match v.as_slice() {
                        [a] => Some((*a, *a)),
                        [a, i, ..] => Some((*a, *i)),
                        _ => None,
                    };
                }
                "no_shadow" => effects.no_shadow = truthy,
                "no_blur" => effects.no_blur = truthy,
                "no_anim" => effects.no_anim = truthy,
                "decorate" => effects.decorate = Some(truthy),
                "float" | "tile" => {}
                other => warn(&format!("hl.window_rule effect {other:?}")),
            }
        }
        Ok(WindowRule { name, enabled, matchers, unmatchable, effects })
    }

    fn matches(&self, f: &Facts) -> bool {
        if !self.enabled.get() || self.unmatchable {
            return false;
        }
        self.matchers.iter().all(|(prop, m)| {
            let text = |s: &str| match m {
                Matcher::Regex { re, negative } => re.is_match(s) != *negative,
                Matcher::Bool(_) => false,
            };
            let flag = |v: bool| match m {
                Matcher::Bool(b) => *b == v,
                Matcher::Regex { re, negative } => re.is_match(if v { "true" } else { "false" }) != *negative,
            };
            match prop {
                Prop::Class | Prop::InitialClass => text(&f.class),
                Prop::Title | Prop::InitialTitle => text(&f.title),
                Prop::Workspace => text(&f.workspace),
                Prop::Float => flag(f.floating),
                Prop::Fullscreen => flag(f.fullscreen),
                Prop::Focus => flag(f.focused),
                Prop::Modal => flag(f.modal),
                Prop::Xwayland | Prop::Pin => flag(false),
            }
        })
    }
}

/// A whole-string regex.
fn regex_matcher(pattern: &str, negative: bool) -> Option<Matcher> {
    Regex::new(&format!("^(?:{pattern})$")).ok().map(|re| Matcher::Regex { re, negative })
}

/// Every matching rule's effects, in order.
pub fn evaluate(rules: &[WindowRule], facts: &Facts) -> Effects {
    let mut out = Effects::default();
    for rule in rules.iter().filter(|r| r.matches(facts)) {
        tracing::trace!(rule = rule.name, class = facts.class, title = facts.title, "window rule matches");
        out.merge(&rule.effects);
    }
    out
}

// ---- move/size expressions ----

/// Values the expressions can use.
pub struct Vars {
    pub monitor: (f64, f64),
    pub window: (f64, f64),
    pub cursor: (f64, f64),
}

/// "x y" to a pair, or None if either part doesn't parse.
pub fn eval_pair(s: &str, vars: &Vars) -> Option<(f64, f64)> {
    let parts: Vec<&str> = s.split_whitespace().filter(|p| *p != "onscreen").collect();
    let [x, y] = parts.as_slice() else { return None };
    Some((eval(x, vars, vars.monitor.0)?, eval(y, vars, vars.monitor.1)?))
}

/// One expression; `percent_of` is what `N%` is a fraction of.
fn eval(s: &str, vars: &Vars, percent_of: f64) -> Option<f64> {
    let mut p = Parser { s: s.as_bytes(), i: 0, vars, percent_of };
    let v = p.sum()?;
    (p.i == p.s.len()).then_some(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    vars: &'a Vars,
    percent_of: f64,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn sum(&mut self) -> Option<f64> {
        let mut v = self.product()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.i += 1;
            let r = self.product()?;
            v = if c == b'+' { v + r } else { v - r };
        }
        Some(v)
    }

    fn product(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        while let Some(c @ (b'*' | b'/')) = self.peek() {
            self.i += 1;
            let r = self.unary()?;
            v = if c == b'*' { v * r } else { v / r };
        }
        Some(v)
    }

    fn unary(&mut self) -> Option<f64> {
        if self.peek() == Some(b'-') {
            self.i += 1;
            return Some(-self.unary()?);
        }
        self.atom()
    }

    fn atom(&mut self) -> Option<f64> {
        let c = self.peek()?;
        if c == b'(' {
            self.i += 1;
            let v = self.sum()?;
            (self.peek() == Some(b')')).then(|| self.i += 1)?;
            return Some(v);
        }
        let start = self.i;
        if c.is_ascii_digit() || c == b'.' {
            while self.peek().is_some_and(|c| c.is_ascii_digit() || c == b'.') {
                self.i += 1;
            }
            let n: f64 = std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()?;
            if self.peek() == Some(b'%') {
                self.i += 1;
                return Some(n / 100.0 * self.percent_of);
            }
            return Some(n);
        }
        while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || c == b'_') {
            self.i += 1;
        }
        let v = self.vars;
        match std::str::from_utf8(&self.s[start..self.i]).ok()? {
            "monitor_w" => Some(v.monitor.0),
            "monitor_h" => Some(v.monitor.1),
            "window_w" => Some(v.window.0),
            "window_h" => Some(v.window.1),
            "cursor_x" => Some(v.cursor.0),
            "cursor_y" => Some(v.cursor.1),
            _ => None,
        }
    }
}

// ---- per-window evaluation ----

/// A window's effects as of its last facts.
type RuleCache = RefCell<Option<(Facts, Rc<Effects>)>>;

/// Where a floating window opened by a rule goes once its size is known.
pub struct Placement {
    pub move_to: Option<String>,
}

impl State {
    /// What rules match on for a window, right now.
    pub fn rule_facts(&self, window: &Window) -> Facts {
        let (class, title) = crate::ipc::class_title(window);
        let workspace = self.workspace_of(window).map(|id| self.wm.workspace_name(id)).unwrap_or_default();
        Facts {
            class,
            title,
            floating: self.is_floating_here(window),
            fullscreen: self.is_fullscreen(window),
            focused: self.focused_window().as_ref() == Some(window),
            modal: window.toplevel().is_some_and(|t| t.parent().is_some()),
            workspace,
        }
    }

    /// The rules' effects for a window with these facts.
    pub fn evaluate_rules(&self, facts: &Facts) -> Effects {
        evaluate(&self.config.window_rules(), facts)
    }

    /// A window's rule effects, evaluated again when its facts change.
    pub fn rule_effects(&self, window: &Window) -> Rc<Effects> {
        let facts = self.rule_facts(window);
        window.user_data().insert_if_missing(|| RuleCache::new(None));
        let cache = window.user_data().get::<RuleCache>().unwrap();
        if let Some((f, e)) = cache.borrow().as_ref() {
            if *f == facts {
                return e.clone();
            }
        }
        let effects = Rc::new(self.evaluate_rules(&facts));
        *cache.borrow_mut() = Some((facts, effects.clone()));
        effects
    }

    /// The variables for a window's `move`/`size` expressions.
    pub fn rule_vars(&self, window_size: (f64, f64)) -> Vars {
        Vars {
            monitor: (self.output_size.w as f64, self.output_size.h as f64),
            window: window_size,
            cursor: (self.pointer_location.x, self.pointer_location.y),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> Vars {
        Vars { monitor: (1000.0, 800.0), window: (300.0, 200.0), cursor: (5.0, 6.0) }
    }

    #[test]
    fn expressions() {
        assert_eq!(eval_pair("20 monitor_h-120", &vars()), Some((20.0, 680.0)));
        assert_eq!(eval_pair("50% 25%", &vars()), Some((500.0, 200.0)));
        assert_eq!(eval_pair("(monitor_w-window_w)/2 -cursor_y", &vars()), Some((350.0, -6.0)));
        assert_eq!(eval_pair("onscreen cursor_x cursor_y", &vars()), Some((5.0, 6.0)));
        assert_eq!(eval_pair("10", &vars()), None);
        assert_eq!(eval_pair("10 bogus", &vars()), None);
    }

    fn rules(src: &str) -> Vec<WindowRule> {
        let lua = mlua::Lua::new();
        let list: Vec<Table> = lua.load(src).eval().unwrap();
        list.iter().map(|t| WindowRule::parse(t, |_| {}).unwrap()).collect()
    }

    #[test]
    fn matching_and_merging() {
        let r = rules(
            r#"return {
                { match = { class = "vte.*" }, float = true, opacity = "0.9 0.8" },
                { match = { title = "negative:sh" }, rounding = 3 },
                { match = { class = ".*", float = true }, no_shadow = true },
                { match = { class = ".*", group = true }, no_blur = true },
            }"#,
        );
        let mut f = Facts { class: "vte-2.91".into(), title: "sh".into(), ..Default::default() };
        let e = evaluate(&r, &f);
        assert_eq!(e.float, Some(true));
        assert_eq!(e.opacity, Some((0.9, 0.8)));
        assert_eq!(e.rounding, None); // title is exactly "sh"
        assert!(!e.no_shadow && !e.no_blur); // not floating yet; `group` never matches
        f.title = "vim".into();
        f.floating = true;
        let e = evaluate(&r, &f);
        assert_eq!(e.rounding, Some(3.0));
        assert!(e.no_shadow);
        // Whole-string matches: "vte" doesn't match "vte-2.91".
        let r = rules(r#"return { { match = { class = "vte" }, float = true } }"#);
        assert_eq!(evaluate(&r, &f).float, None);
    }
}
