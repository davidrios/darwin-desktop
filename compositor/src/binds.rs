//! Keybinds: Hyprland's key strings ("SUPER + SHIFT + 1") and the actions
//! they trigger. SUPER is Command; ALT is left Option (`layout.rs`).

use std::{cell::Cell, rc::Rc};

use smithay::input::keyboard::{Keysym, ModifiersState, xkb};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub logo: bool,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl From<&ModifiersState> for Mods {
    fn from(m: &ModifiersState) -> Mods {
        Mods { logo: m.logo, shift: m.shift, ctrl: m.ctrl, alt: m.alt }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn parse(s: &str) -> Option<Direction> {
        Some(match s.to_ascii_lowercase().as_str() {
            "l" | "left" => Direction::Left,
            "r" | "right" => Direction::Right,
            "u" | "t" | "up" | "top" => Direction::Up,
            "d" | "b" | "down" | "bottom" => Direction::Down,
            _ => return None,
        })
    }
}

/// Hyprland workspace selectors we understand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceTarget {
    Id(i32),
    /// `+1`, `-1`, `e+1`, `e-1`: relative, `existing` skipping empty ones.
    Relative { delta: i32, existing: bool },
    Previous,
    /// `special:<name>` (`special` alone is `special:special`).
    Special(String),
}

impl WorkspaceTarget {
    pub fn parse(s: &str) -> Option<WorkspaceTarget> {
        let s = s.trim();
        if s == "previous" {
            return Some(WorkspaceTarget::Previous);
        }
        if s == "special" {
            return Some(WorkspaceTarget::Special("special".into()));
        }
        if let Some(name) = s.strip_prefix("special:").filter(|n| !n.is_empty()) {
            return Some(WorkspaceTarget::Special(name.into()));
        }
        if let Some(rest) = s.strip_prefix('e').or_else(|| s.strip_prefix('m')) {
            if let Ok(delta) = rest.parse::<i32>() {
                return Some(WorkspaceTarget::Relative { delta, existing: true });
            }
        }
        if s.starts_with('+') || s.starts_with('-') {
            return s.parse::<i32>().ok().map(|delta| WorkspaceTarget::Relative { delta, existing: false });
        }
        s.parse::<i32>().ok().filter(|&id| id > 0).map(WorkspaceTarget::Id)
    }
}

/// What a bind (or `hl.dispatch`) does.
#[derive(Clone, Debug)]
pub enum Action {
    Exec(String),
    Close,
    ToggleFloat,
    ToggleSplit,
    Fullscreen,
    Focus(Direction),
    FocusWorkspace(WorkspaceTarget),
    MoveWindow(Direction),
    MoveToWorkspace { target: WorkspaceTarget, follow: bool },
    /// `hl.dsp.group.toggle()`.
    ToggleGroup,
    /// `hl.dsp.group.next()` (+1) / `prev()` (-1).
    GroupCycle(i32),
    /// `hl.dsp.group.move_window({ forward })`.
    GroupMoveWindow { forward: bool },
    /// `hl.dsp.group.lock({ action })`: Some(on), or None to toggle.
    LockGroup(Option<bool>),
    /// `hl.dsp.window.move({ direction, group_aware = true })`.
    MoveWindowOrGroup(Direction),
    /// `hl.dsp.workspace.toggle_special(name)`: show or hide a special workspace.
    ToggleSpecial(String),
    /// `hl.dsp.window.drag()` on a mouse bind: move the window under the pointer.
    MouseDrag,
    /// `hl.dsp.window.resize()` on a mouse bind: resize it from the nearest corner.
    MouseResize,
    Exit,
    /// A dispatcher we don't implement yet: warn when used.
    Unsupported(String),
}

/// A mouse button or wheel direction in a bind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKey {
    /// `mouse:272`: a Linux input button code (272 left, 273 right, 274 middle).
    Button(u32),
    /// `mouse_down` / `mouse_up` / `mouse_left` / `mouse_right`: the wheel.
    WheelDown,
    WheelUp,
    WheelLeft,
    WheelRight,
}

/// A parsed key string.
#[derive(Clone, Debug, PartialEq)]
pub struct Chord {
    pub mods: Mods,
    /// Lowercased keysym, so "Q" matches the q key (NoSymbol for mouse chords).
    pub keysym: Keysym,
    pub mouse: Option<MouseKey>,
}

pub fn parse_chord(s: &str) -> Result<Chord, String> {
    let parts: Vec<&str> = s.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let Some((key, mods)) = parts.split_last() else { return Err(format!("empty key string {s:?}")) };
    let mut m = Mods::default();
    for token in mods.iter().flat_map(|p| p.split(|c: char| c.is_whitespace() || c == '_')).filter(|t| !t.is_empty()) {
        match token.to_ascii_uppercase().as_str() {
            "SUPER" | "WIN" | "LOGO" | "MOD4" | "META" | "CMD" | "COMMAND" => m.logo = true,
            "SHIFT" => m.shift = true,
            "CTRL" | "CONTROL" => m.ctrl = true,
            "ALT" | "MOD1" | "OPTION" => m.alt = true,
            other => return Err(format!("unknown modifier {other:?} in {s:?}")),
        }
    }
    if key.starts_with("mouse") {
        let mouse = match key.to_ascii_lowercase().as_str() {
            "mouse_down" => MouseKey::WheelDown,
            "mouse_up" => MouseKey::WheelUp,
            "mouse_left" => MouseKey::WheelLeft,
            "mouse_right" => MouseKey::WheelRight,
            k => match k.strip_prefix("mouse:").and_then(|n| n.parse().ok()) {
                Some(code) => MouseKey::Button(code),
                None => return Err(format!("unknown mouse key {key:?} in {s:?}")),
            },
        };
        return Ok(Chord { mods: m, keysym: Keysym::new(keysyms::KEY_NoSymbol), mouse: Some(mouse) });
    }
    let keysym = xkb::keysym_from_name(key, xkb::KEYSYM_CASE_INSENSITIVE);
    if keysym.raw() == keysyms::KEY_NoSymbol {
        return Err(format!("unknown key {key:?} in {s:?}"));
    }
    Ok(Chord { mods: m, keysym: lower(keysym), mouse: None })
}

use smithay::input::keyboard::keysyms;

/// Latin letters (ASCII and Latin-1) to lowercase; other keysyms unchanged.
pub fn lower(k: Keysym) -> Keysym {
    let raw = k.raw();
    match raw {
        0x41..=0x5a | 0xc0..=0xd6 | 0xd8..=0xde => Keysym::new(raw + 0x20),
        _ => k,
    }
}

/// A bind from the config: the chord and what it runs.
pub struct Bind {
    pub chord: Chord,
    pub run: BindRun,
    pub enabled: Rc<Cell<bool>>,
}

pub enum BindRun {
    Action(Action),
    /// A Lua function, called on press.
    Lua(mlua::Function),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords() {
        let c = parse_chord("SUPER + SHIFT + 1").unwrap();
        assert_eq!(c.mods, Mods { logo: true, shift: true, ctrl: false, alt: false });
        assert_eq!(c.keysym.raw(), keysyms::KEY_1);
        let q = parse_chord("SUPER + Q").unwrap();
        assert_eq!(q.keysym.raw(), keysyms::KEY_q);
        assert_eq!(parse_chord("SUPER + left").unwrap().keysym.raw(), keysyms::KEY_Left);
        assert_eq!(parse_chord("XF86AudioMute").unwrap().mods, Mods::default());
        assert_eq!(parse_chord("SUPER + mouse:272").unwrap().mouse, Some(MouseKey::Button(272)));
        assert_eq!(parse_chord("SUPER + mouse_down").unwrap().mouse, Some(MouseKey::WheelDown));
        assert!(parse_chord("SUPER + mouse:x").is_err());
        assert!(parse_chord("HYPER + Q").is_err());
    }

    #[test]
    fn workspaces() {
        assert_eq!(WorkspaceTarget::parse("3"), Some(WorkspaceTarget::Id(3)));
        assert_eq!(WorkspaceTarget::parse("e+1"), Some(WorkspaceTarget::Relative { delta: 1, existing: true }));
        assert_eq!(WorkspaceTarget::parse("-1"), Some(WorkspaceTarget::Relative { delta: -1, existing: false }));
        assert_eq!(WorkspaceTarget::parse("special:magic"), Some(WorkspaceTarget::Special("magic".into())));
        assert_eq!(WorkspaceTarget::parse("special"), Some(WorkspaceTarget::Special("special".into())));
    }
}
