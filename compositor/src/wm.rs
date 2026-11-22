//! Window management: workspaces, tiling, focus, and the bind actions.
//!
//! Each workspace has a dwindle tree of tiled windows, a list of floating
//! ones and at most one fullscreen window. Only the active workspace's
//! windows are mapped in Smithay's `Space` (so only they are drawn, hit and
//! sent frame callbacks). Tile geometry follows Hyprland: the work area is the
//! output minus `gaps_out`; a tile's edges inside the work area give up
//! `gaps_in`; the border is reserved inside the tile.

use std::{collections::BTreeMap, process::Command};

use smithay::{
    desktop::Window,
    reexports::{wayland_protocols::xdg::shell::server::xdg_toplevel, wayland_server::protocol::wl_surface::WlSurface},
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Size},
    wayland::seat::WaylandFocus,
};

use crate::{
    binds::{Action, Direction, WorkspaceTarget},
    dwindle::{Dwindle, Rect},
    state::State,
};

/// The rectangle a tiled window may draw in (its tile minus gaps and border),
/// kept in the window's user data. A client bigger than its tile (a minimum
/// size larger than the tile) is clipped to it, as Hyprland does.
#[derive(Default)]
pub struct TileClip(pub std::cell::Cell<Option<Rectangle<i32, Logical>>>);

pub fn set_clip(window: &Window, clip: Option<Rectangle<i32, Logical>>) {
    window.user_data().insert_if_missing(TileClip::default);
    window.user_data().get::<TileClip>().unwrap().0.set(clip);
}

pub fn clip_of(window: &Window) -> Option<Rectangle<i32, Logical>> {
    window.user_data().get::<TileClip>().and_then(|c| c.0.get())
}

#[derive(Default)]
pub struct Workspace {
    pub tiled: Dwindle<Window>,
    pub floating: Vec<Window>,
    pub fullscreen: Option<Window>,
    /// Most recently focused last.
    history: Vec<Window>,
}

impl Workspace {
    pub fn windows(&self) -> Vec<Window> {
        let mut all: Vec<Window> = self.tiled.tiles().into_iter().map(|(w, _)| w).collect();
        all.extend(self.floating.iter().cloned());
        all
    }

    pub fn is_empty(&self) -> bool {
        self.tiled.is_empty() && self.floating.is_empty()
    }

    fn contains(&self, w: &Window) -> bool {
        self.tiled.contains(w) || self.floating.contains(w)
    }

    fn forget(&mut self, w: &Window) {
        self.tiled.remove(w);
        self.floating.retain(|f| f != w);
        self.history.retain(|h| h != w);
        if self.fullscreen.as_ref() == Some(w) {
            self.fullscreen = None;
        }
    }

    fn last_focused(&self) -> Option<Window> {
        self.history.iter().rev().find(|w| self.contains(w)).cloned().or_else(|| self.windows().into_iter().next())
    }
}

pub struct Wm {
    pub workspaces: BTreeMap<i32, Workspace>,
    pub active: i32,
    previous: i32,
    /// Toplevels created but not yet committed (no size decided yet).
    pub pending: Vec<Window>,
    /// A workspace switch animating.
    pub transition: Option<crate::anim::Transition>,
}

impl Default for Wm {
    fn default() -> Self {
        let mut workspaces = BTreeMap::new();
        workspaces.insert(1, Workspace::default());
        Wm { workspaces, active: 1, previous: 1, pending: Vec::new(), transition: None }
    }
}

fn sticks(a: f64, b: f64) -> bool {
    (a - b).abs() < 2.0
}

impl State {
    /// Any toplevel we know, on any workspace, by its surface.
    pub fn find_window(&self, surface: &WlSurface) -> Option<Window> {
        let matches = |w: &Window| w.wl_surface().is_some_and(|s| *s == *surface);
        self.wm
            .pending
            .iter()
            .find(|w| matches(w))
            .cloned()
            .or_else(|| self.wm.workspaces.values().flat_map(|ws| ws.windows()).find(|w| matches(w)))
    }

    /// Whether a window is the active workspace's fullscreen window.
    pub fn is_fullscreen(&self, w: &Window) -> bool {
        self.wm.workspaces.get(&self.wm.active).is_some_and(|ws| ws.fullscreen.as_ref() == Some(w))
    }

    /// Fullscreen a window on its workspace (one per workspace), or undo it.
    pub fn set_fullscreen(&mut self, w: &Window, on: bool) {
        let Some(id) = self.workspace_of(w) else { return };
        let ws = self.wm.workspaces.get_mut(&id).unwrap();
        if on {
            ws.fullscreen = Some(w.clone());
        } else if ws.fullscreen.as_ref() == Some(w) {
            ws.fullscreen = None;
        } else {
            return;
        }
        if id == self.wm.active {
            self.relayout();
        }
    }

    /// Whether a window floats on the active workspace.
    pub fn is_floating_here(&self, w: &Window) -> bool {
        self.active_ws_ref().floating.contains(w)
    }

    fn workspace_of(&self, w: &Window) -> Option<i32> {
        self.wm.workspaces.iter().find(|(_, ws)| ws.contains(w)).map(|(id, _)| *id)
    }

    fn active_ws(&mut self) -> &mut Workspace {
        self.wm.workspaces.entry(self.wm.active).or_default()
    }

    pub fn focused_window(&self) -> Option<Window> {
        let surface = self.seat.get_keyboard()?.current_focus()?;
        self.find_window(&surface)
    }

    /// The output minus `gaps_out`.
    fn work_area(&self) -> Rect {
        let g = self.config.settings.gaps_out;
        let (w, h) = (self.output_size.w as f64, self.output_size.h as f64);
        Rect::new(g.left, g.top, w - g.left - g.right, h - g.top - g.bottom)
    }

    /// A tile's window rectangle: inner gaps and the border taken off.
    fn window_rect(&self, tile: Rect, area: Rect) -> Rectangle<i32, Logical> {
        let s = &self.config.settings;
        let gi = s.gaps_in;
        let left = if sticks(tile.x, area.x) { 0.0 } else { gi.left };
        let top = if sticks(tile.y, area.y) { 0.0 } else { gi.top };
        let right = if sticks(tile.x + tile.w, area.x + area.w) { 0.0 } else { gi.right };
        let bottom = if sticks(tile.y + tile.h, area.y + area.h) { 0.0 } else { gi.bottom };
        let b = s.border_size.max(0) as f64;
        let x = tile.x + left + b;
        let y = tile.y + top + b;
        let w = (tile.w - left - right - 2.0 * b).max(1.0);
        let h = (tile.h - top - bottom - 2.0 * b).max(1.0);
        Rectangle::new((x.round() as i32, y.round() as i32).into(), (w.round() as i32, h.round() as i32).into())
    }

    /// A toplevel's first commit: tile or float it on the active workspace.
    pub fn manage(&mut self, window: Window) {
        self.wm.pending.retain(|w| *w != window);
        let Some(toplevel) = window.toplevel() else { return };
        crate::anim::mark_fresh(&window);
        let fixed_size = {
            let (min, max) = smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
                let mut cached = states.cached_state.get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
                let c = cached.current();
                (c.min_size, c.max_size)
            });
            min.w > 0 && min == max
        };
        let float = toplevel.parent().is_some() || fixed_size;
        let pointer = (self.pointer_location.x, self.pointer_location.y);
        let area = self.work_area();
        let opening_on = if self.config.settings.dwindle.use_active_for_splits {
            self.focused_window().filter(|w| self.active_ws_ref().tiled.contains(w))
        } else {
            self.active_ws_ref().tiled.at(pointer.0, pointer.1)
        };
        let cfg = self.config.settings.dwindle.clone();
        // Split the way dwindle would, unless the halves would be smaller than
        // either window's minimum size: then the other way, else float.
        let split = if float { None } else { self.split_that_fits(&window, opening_on.as_ref(), area) };
        let ws = self.active_ws();
        match split {
            Some(stacked) if !float => {
                ws.tiled.insert_split(window.clone(), opening_on.as_ref(), Some(pointer), area, &cfg, stacked);
            }
            _ => {
                if !float {
                    tracing::debug!("no room to tile a window at its minimum size: floating it");
                }
                ws.floating.push(window.clone());
            }
        }
        self.relayout();
        self.focus(Some(window), SERIAL_COUNTER.next_serial());
    }

    /// How to split for a new window: `Some(None)` dwindle's own choice,
    /// `Some(Some(stacked))` a forced direction, `None` no split fits both
    /// windows' minimum sizes.
    fn split_that_fits(&self, new: &Window, opening_on: Option<&Window>, area: Rect) -> Option<Option<bool>> {
        let ws = self.active_ws_ref();
        let target = opening_on.cloned().or_else(|| ws.tiled.tiles().first().map(|(w, _)| w.clone()));
        let Some(target) = target else { return Some(None) }; // first window: fills the area
        let Some(rect) = ws.tiled.rect_of(&target) else { return Some(None) };
        let cfg = &self.config.settings.dwindle;
        let need = {
            let (a, b) = (min_size(new), min_size(&target));
            Size::<i32, Logical>::from((a.w.max(b.w), a.h.max(b.h)))
        };
        let fits = |stacked: bool| {
            let first = if stacked { rect.h / 2.0 * cfg.default_split_ratio } else { rect.w / 2.0 * cfg.default_split_ratio };
            let halves = if stacked {
                [Rect::new(rect.x, rect.y, rect.w, first), Rect::new(rect.x, rect.y + first, rect.w, rect.h - first)]
            } else {
                [Rect::new(rect.x, rect.y, first, rect.h), Rect::new(rect.x + first, rect.y, rect.w - first, rect.h)]
            };
            halves.iter().all(|h| {
                let r = self.window_rect(*h, area);
                r.size.w >= need.w && r.size.h >= need.h
            })
        };
        let default_stacked = !(rect.w > rect.h * cfg.split_width_multiplier);
        if fits(default_stacked) {
            Some(None)
        } else if fits(!default_stacked) {
            Some(Some(!default_stacked))
        } else {
            None
        }
    }

    fn active_ws_ref(&self) -> &Workspace {
        &self.wm.workspaces[&self.wm.active]
    }

    /// A toplevel went away.
    pub fn unmanage(&mut self, window: &Window) {
        self.wm.pending.retain(|w| w != window);
        let was_active = self.workspace_of(window) == Some(self.wm.active);
        if was_active {
            self.animate_close(window);
        }
        for ws in self.wm.workspaces.values_mut() {
            ws.forget(window);
        }
        self.space.unmap_elem(window);
        if was_active {
            self.relayout();
            let next = self.active_ws_ref().last_focused();
            self.focus(next, SERIAL_COUNTER.next_serial());
        }
    }

    /// Lay out and map the active workspace.
    pub fn relayout(&mut self) {
        let area = self.work_area();
        let cfg = self.config.settings.dwindle.clone();
        let output = Rectangle::from_size(self.output_size);
        let active = self.wm.active;
        let (tiles, floating, fullscreen) = {
            let ws = self.wm.workspaces.entry(active).or_default();
            ws.tiled.recalc(area, &cfg);
            (ws.tiled.tiles(), ws.floating.clone(), ws.fullscreen.clone())
        };

        for (window, tile) in tiles {
            if fullscreen.as_ref() == Some(&window) {
                continue;
            }
            let rect = self.window_rect(tile, area);
            tracing::debug!(?tile, ?rect, "tile");
            configure(&window, Some(rect.size), true, false);
            set_clip(&window, Some(rect));
            self.space.map_element(window, rect.loc, false);
        }
        for window in floating {
            if fullscreen.as_ref() == Some(&window) {
                continue;
            }
            configure(&window, None, false, false);
            set_clip(&window, None);
            let loc = self.space.element_location(&window).unwrap_or_else(|| {
                let size = window.geometry().size;
                ((output.size.w - size.w) / 2, (output.size.h - size.h) / 2).into()
            });
            self.space.map_element(window, loc, false);
        }
        if let Some(window) = fullscreen {
            configure(&window, Some(output.size), false, true);
            set_clip(&window, Some(output));
            self.space.map_element(window, (0, 0), true);
        }
        // Floating windows stay above the tiles.
        let floating: Vec<Window> = self.active_ws_ref().floating.clone();
        for w in floating {
            self.space.raise_element(&w, false);
        }
        self.needs_redraw = true;
    }

    /// Keyboard focus to a window (or nothing), with activation and history.
    pub fn focus(&mut self, window: Option<Window>, serial: smithay::utils::Serial) {
        if let Some(w) = &window {
            if let Some(id) = self.workspace_of(w) {
                let ws = self.wm.workspaces.get_mut(&id).unwrap();
                ws.history.retain(|h| h != w);
                ws.history.push(w.clone());
                if ws.floating.contains(w) {
                    self.space.raise_element(w, false);
                }
            }
        }
        for w in self.space.elements() {
            let active = window.as_ref() == Some(w);
            if w.set_activated(active) {
                if let Some(t) = w.toplevel() {
                    if t.is_initial_configure_sent() {
                        t.send_pending_configure();
                    }
                }
            }
        }
        let surface = window.and_then(|w| w.wl_surface().map(|s| s.into_owned()));
        if let Some(keyboard) = self.seat.get_keyboard() {
            if keyboard.current_focus() != surface {
                keyboard.set_focus(self, surface, serial);
            }
        }
        self.needs_redraw = true;
    }

    pub fn switch_workspace(&mut self, id: i32) {
        if id == self.wm.active || id < 1 {
            return;
        }
        for w in self.active_ws_ref().windows() {
            self.space.unmap_elem(&w);
        }
        self.wm.previous = self.wm.active;
        // Drop the workspace we leave if it's empty (as Hyprland does).
        if self.active_ws_ref().is_empty() {
            self.wm.workspaces.remove(&self.wm.active);
        }
        let from = self.wm.active;
        self.wm.active = id;
        self.wm.workspaces.entry(id).or_default();
        self.relayout();
        self.closing.clear();
        self.animate_workspace_switch(from);
        let next = self.active_ws_ref().last_focused();
        self.focus(next, SERIAL_COUNTER.next_serial());
        tracing::debug!("workspace {id}");
    }

    fn resolve_workspace(&self, target: &WorkspaceTarget) -> Option<i32> {
        match target {
            WorkspaceTarget::Id(id) => Some(*id),
            WorkspaceTarget::Previous => Some(self.wm.previous),
            WorkspaceTarget::Relative { delta, existing: false } => Some((self.wm.active + delta).max(1)),
            WorkspaceTarget::Relative { delta, existing: true } => {
                let ids: Vec<i32> =
                    self.wm.workspaces.iter().filter(|(id, ws)| !ws.is_empty() || **id == self.wm.active).map(|(id, _)| *id).collect();
                let pos = ids.iter().position(|id| *id == self.wm.active)? as i32;
                let n = ids.len() as i32;
                Some(ids[((pos + delta) % n + n) as usize % n as usize])
            }
        }
    }

    fn move_to_workspace(&mut self, window: Window, target: i32, follow: bool) {
        let Some(from) = self.workspace_of(&window) else { return };
        if from == target {
            return;
        }
        let floating = self.wm.workspaces[&from].floating.contains(&window);
        self.wm.workspaces.get_mut(&from).unwrap().forget(&window);
        self.space.unmap_elem(&window);
        let area = self.work_area();
        let cfg = self.config.settings.dwindle.clone();
        let dest = self.wm.workspaces.entry(target).or_default();
        if floating {
            dest.floating.push(window.clone());
        } else {
            let opening_on = dest.last_focused();
            dest.tiled.insert(window.clone(), opening_on.as_ref(), None, area, &cfg);
        }
        dest.history.push(window.clone());
        if follow {
            self.switch_workspace(target);
            self.focus(Some(window), SERIAL_COUNTER.next_serial());
        } else {
            self.relayout();
            let next = self.active_ws_ref().last_focused();
            self.focus(next, SERIAL_COUNTER.next_serial());
        }
    }

    /// The nearest window on the active workspace in a direction.
    fn neighbour(&self, from: &Window, dir: Direction) -> Option<Window> {
        let geo = |w: &Window| {
            let loc = self.space.element_location(w)?;
            Some(Rectangle::new(loc, w.geometry().size))
        };
        let a = geo(from)?;
        let (ax, ay) = (a.loc.x as f64 + a.size.w as f64 / 2.0, a.loc.y as f64 + a.size.h as f64 / 2.0);
        self.active_ws_ref()
            .windows()
            .into_iter()
            .filter(|w| w != from)
            .filter_map(|w| geo(&w).map(|r| (w, r)))
            .filter(|(_, r)| match dir {
                Direction::Left => r.loc.x + r.size.w <= a.loc.x + 1,
                Direction::Right => r.loc.x >= a.loc.x + a.size.w - 1,
                Direction::Up => r.loc.y + r.size.h <= a.loc.y + 1,
                Direction::Down => r.loc.y >= a.loc.y + a.size.h - 1,
            })
            .min_by(|(_, r1), (_, r2)| {
                let d = |r: &Rectangle<i32, Logical>| {
                    let (cx, cy) = (r.loc.x as f64 + r.size.w as f64 / 2.0, r.loc.y as f64 + r.size.h as f64 / 2.0);
                    // Distance along the direction, plus misalignment across it (weighted).
                    match dir {
                        Direction::Left | Direction::Right => (cx - ax).abs() + 2.0 * (cy - ay).abs(),
                        Direction::Up | Direction::Down => (cy - ay).abs() + 2.0 * (cx - ax).abs(),
                    }
                };
                d(r1).total_cmp(&d(r2))
            })
            .map(|(w, _)| w)
    }

    pub fn spawn(&mut self, cmd: &str) {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(cmd)
            .env("WAYLAND_DISPLAY", &self.socket_name)
            .env("XDG_SESSION_TYPE", "wayland")
            .envs(self.config.env());
        match command.spawn() {
            Ok(child) => {
                tracing::info!("exec {cmd:?} (pid {})", child.id());
                self.children.push(child);
            }
            Err(err) => tracing::error!("exec {cmd:?}: {err}"),
        }
    }

    pub fn run_action(&mut self, action: Action) {
        tracing::debug!(?action, "action");
        let focused = self.focused_window();
        match action {
            Action::Exec(cmd) => self.spawn(&cmd),
            Action::Exit => self.quit_requested = true,
            Action::Close => {
                if let Some(t) = focused.as_ref().and_then(Window::toplevel) {
                    t.send_close();
                }
            }
            Action::FocusWorkspace(target) => {
                if let Some(id) = self.resolve_workspace(&target) {
                    self.switch_workspace(id);
                }
            }
            Action::MoveToWorkspace { target, follow } => {
                if let (Some(w), Some(id)) = (focused, self.resolve_workspace(&target)) {
                    self.move_to_workspace(w, id, follow);
                }
            }
            Action::Focus(dir) => {
                let next = match &focused {
                    Some(w) => self.neighbour(w, dir),
                    None => self.active_ws_ref().last_focused(),
                };
                if next.is_some() {
                    self.focus(next, SERIAL_COUNTER.next_serial());
                }
            }
            Action::MoveWindow(dir) => {
                let Some(w) = focused else { return };
                if let Some(other) = self.neighbour(&w, dir) {
                    let ws = self.active_ws();
                    if ws.tiled.contains(&w) && ws.tiled.contains(&other) {
                        ws.tiled.swap(&w, &other);
                        self.relayout();
                    }
                }
            }
            Action::ToggleSplit => {
                if let Some(w) = focused {
                    self.active_ws().tiled.toggle_split(&w);
                    self.relayout();
                }
            }
            Action::ToggleFloat => {
                let Some(w) = focused else { return };
                let area = self.work_area();
                let cfg = self.config.settings.dwindle.clone();
                let ws = self.active_ws();
                if ws.tiled.contains(&w) {
                    ws.tiled.remove(&w);
                    ws.floating.push(w.clone());
                    // Float at a comfortable size, centred.
                    let size: Size<i32, Logical> = ((area.w * 0.6) as i32, (area.h * 0.6) as i32).into();
                    configure(&w, Some(size), false, false);
                    set_clip(&w, None);
                    let loc: Point<i32, Logical> =
                        ((self.output_size.w - size.w) / 2, (self.output_size.h - size.h) / 2).into();
                    self.space.map_element(w.clone(), loc, true);
                } else if ws.floating.contains(&w) {
                    ws.floating.retain(|f| *f != w);
                    ws.tiled.insert(w.clone(), None, None, area, &cfg);
                }
                self.relayout();
            }
            Action::Fullscreen => {
                let Some(w) = focused else { return };
                let on = !self.is_fullscreen(&w);
                self.set_fullscreen(&w, on);
            }
            Action::Unsupported(name) => tracing::warn!("{name} is not supported yet"),
        }
    }
}

/// A toplevel's minimum size (xdg_toplevel.set_min_size; 0 when unset).
fn min_size(window: &Window) -> Size<i32, Logical> {
    let Some(t) = window.toplevel() else { return (0, 0).into() };
    smithay::wayland::compositor::with_states(t.wl_surface(), |states| {
        states.cached_state.get::<smithay::wayland::shell::xdg::SurfaceCachedState>().current().min_size
    })
}

/// Size and tiled/fullscreen states for a toplevel.
fn configure(window: &Window, size: Option<Size<i32, Logical>>, tiled: bool, fullscreen: bool) {
    let Some(t) = window.toplevel() else { return };
    t.with_pending_state(|s| {
        if size.is_some() || tiled || fullscreen {
            s.size = size;
        }
        s.states.unset(xdg_toplevel::State::Maximized);
        for state in [
            xdg_toplevel::State::TiledLeft,
            xdg_toplevel::State::TiledRight,
            xdg_toplevel::State::TiledTop,
            xdg_toplevel::State::TiledBottom,
        ] {
            if tiled {
                s.states.set(state);
            } else {
                s.states.unset(state);
            }
        }
        if fullscreen {
            s.states.set(xdg_toplevel::State::Fullscreen);
        } else {
            s.states.unset(xdg_toplevel::State::Fullscreen);
        }
    });
    if t.is_initial_configure_sent() {
        t.send_pending_configure();
    }
}
