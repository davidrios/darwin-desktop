//! Window groups, as Hyprland's: windows stacked as tabs in one place.
//!
//! A group lives in a workspace (`wm::Group`): its current member holds the
//! group's tile (or floating spot); the others are hidden. A tab bar above it
//! (`group.groupbar`) shows a tab per member: a coloured indicator (the
//! active colour for the focused window) under its title. A grouped window
//! gets the `group.col.border_*` colours.
//!
//! `hl.dsp.group.toggle()` makes the focused window a group of one, or
//! breaks its group back into tiles; with `group.auto_group` new windows join
//! the focused window's group. `group.next()`/`prev()` change the tab,
//! `group.move_window({ forward })` reorders it, `group.lock()` stops windows
//! joining. `window.move({ direction, group_aware = true })` (Hyprland's
//! `movewindoworgroup`) moves the window out of its group, into a group next
//! to it, or like a plain move. Clicking a tab shows that window.

use smithay::{desktop::Window, utils::SERIAL_COUNTER};

use crate::{
    binds::{Action, Direction},
    dwindle::Rect,
    state::State,
};

/// How a group's tab bar looks for one frame.
#[derive(Clone, Debug)]
pub struct GroupLook {
    pub titles: Vec<String>,
    /// The member with the keyboard focus, if any.
    pub focused: Option<usize>,
    pub locked: bool,
}

impl State {
    /// The space a window's tab bar takes above it (0 if it isn't a shown group member).
    pub fn groupbar_height(&self, window: &Window) -> f64 {
        let bar = &self.config.settings.group.bar;
        let Some(id) = self.workspace_of(window) else { return 0.0 };
        match self.wm.workspaces[&id].group_of(window) {
            Some(g) if g.current() == window && !(bar.disable_when_only && g.members.len() == 1) => bar.reserved(),
            _ => 0.0,
        }
    }

    /// A shown group member's tab bar contents.
    pub fn group_look(&self, window: &Window) -> Option<GroupLook> {
        let id = self.workspace_of(window)?;
        let g = self.wm.workspaces[&id].group_of(window)?;
        if g.current() != window || self.groupbar_height(window) <= 0.0 {
            return None;
        }
        let focused = self.focused_window();
        Some(GroupLook {
            titles: g.members.iter().map(|m| crate::ipc::class_title(m).1).collect(),
            focused: g.members.iter().position(|m| Some(m) == focused.as_ref()),
            locked: g.locked,
        })
    }

    /// Whether a window is in a group, and if so whether it's locked.
    pub fn group_locked(&self, window: &Window) -> Option<bool> {
        let id = self.workspace_of(window)?;
        self.wm.workspaces[&id].group_of(window).map(|g| g.locked)
    }

    /// Show a hidden group member in its group's place (before focusing it).
    pub fn reveal_group_member(&mut self, window: &Window) {
        let Some(id) = self.workspace_of(window) else { return };
        let ws = self.wm.workspaces.get_mut(&id).unwrap();
        let Some(gi) = ws.group_index(window) else { return };
        let index = ws.groups[gi].members.iter().position(|m| m == window).unwrap();
        if let Some(old) = ws.show_member(gi, index) {
            self.space.unmap_elem(&old);
            if self.wm.visible(id) {
                self.relayout();
            }
        }
    }

    /// `group.auto_group`: a new window joins the focused window's unlocked
    /// group on the focused workspace. True if it did.
    pub fn auto_group(&mut self, window: &Window) -> bool {
        if !self.config.settings.group.auto_group {
            return false;
        }
        let Some(focused) = self.focused_window() else { return false };
        let id = self.wm.focused_id();
        let after_current = self.config.settings.group.insert_after_current;
        let Some(ws) = self.wm.workspaces.get_mut(&id) else { return false };
        let Some(gi) = ws.group_index(&focused) else { return false };
        if ws.groups[gi].locked {
            return false;
        }
        let g = &mut ws.groups[gi];
        let at = if after_current { g.current + 1 } else { g.members.len() };
        g.members.insert(at, window.clone());
        if let Some(old) = ws.show_member(gi, at) {
            self.space.unmap_elem(&old);
        }
        true
    }

    /// `group.toggle()`: group the focused window on its own, or break its
    /// group up into tiles next to it.
    fn toggle_group(&mut self) {
        let Some(w) = self.focused_window() else { return };
        let Some(id) = self.workspace_of(&w) else { return };
        let area = self.work_area();
        let cfg = self.config.settings.dwindle.clone();
        let ws = self.wm.workspaces.get_mut(&id).unwrap();
        match ws.group_index(&w) {
            None => ws.groups.push(crate::wm::Group { members: vec![w.clone()], current: 0, locked: false }),
            Some(gi) => {
                let g = ws.groups.remove(gi);
                let place = g.current().clone();
                let floating = ws.floating.contains(&place);
                for m in g.members.iter().filter(|m| **m != place) {
                    if floating {
                        ws.floating.push(m.clone());
                    } else {
                        ws.tiled.insert(m.clone(), Some(&place), None, area, &cfg);
                    }
                }
            }
        }
        self.relayout();
    }

    /// `group.next()` / `prev()`: the next tab (wrapping).
    fn group_cycle(&mut self, delta: i32) {
        let Some(w) = self.focused_window() else { return };
        let Some(id) = self.workspace_of(&w) else { return };
        let ws = &self.wm.workspaces[&id];
        let Some(g) = ws.group_of(&w) else { return };
        let n = g.members.len() as i32;
        let next = g.members[(((g.current as i32 + delta) % n + n) % n) as usize].clone();
        self.reveal_group_member(&next);
        self.focus(Some(next), SERIAL_COUNTER.next_serial());
    }

    /// `group.move_window({ forward })`: move the tab along (wrapping).
    fn group_move_window(&mut self, forward: bool) {
        let Some(w) = self.focused_window() else { return };
        let Some(id) = self.workspace_of(&w) else { return };
        let ws = self.wm.workspaces.get_mut(&id).unwrap();
        let Some(gi) = ws.group_index(&w) else { return };
        let g = &mut ws.groups[gi];
        let n = g.members.len();
        let from = g.current;
        let to = if forward { (from + 1) % n } else { (from + n - 1) % n };
        g.members.swap(from, to);
        g.current = to;
        self.needs_redraw = true;
    }

    /// `group.lock({ action })`: None toggles.
    fn group_lock(&mut self, on: Option<bool>) {
        let Some(w) = self.focused_window() else { return };
        let Some(id) = self.workspace_of(&w) else { return };
        let ws = self.wm.workspaces.get_mut(&id).unwrap();
        if let Some(gi) = ws.group_index(&w) {
            let g = &mut ws.groups[gi];
            g.locked = on.unwrap_or(!g.locked);
            self.needs_redraw = true;
        }
    }

    /// Hyprland's `movewindoworgroup`.
    fn move_window_or_group(&mut self, dir: Direction) {
        let Some(w) = self.focused_window() else { return };
        let Some(id) = self.workspace_of(&w) else { return };
        let size = self.wm.workspaces[&id].group_of(&w).map(|g| g.members.len()).unwrap_or(0);
        if size > 1 {
            // Out of the group, into a tile on that side of it.
            let area = self.work_area();
            let mut cfg = self.config.settings.dwindle.clone();
            cfg.force_split = 0; // the side comes from the point below
            let ws = self.wm.workspaces.get_mut(&id).unwrap();
            let gi = ws.group_index(&w).unwrap();
            ws.forget(&w);
            ws.history.push(w.clone());
            // The group (still there: it had others) now shows another member.
            let place = Some(ws.groups[gi].current().clone());
            match place.as_ref().and_then(|p| ws.tiled.rect_of(p).map(|r| (p, r))) {
                Some((p, r)) => {
                    let (cx, cy) = (r.x + r.w / 2.0, r.y + r.h / 2.0);
                    let (point, stacked) = match dir {
                        Direction::Left => ((r.x + 1.0, cy), false),
                        Direction::Right => ((r.x + r.w - 1.0, cy), false),
                        Direction::Up => ((cx, r.y + 1.0), true),
                        Direction::Down => ((cx, r.y + r.h - 1.0), true),
                    };
                    let p = p.clone();
                    ws.tiled.insert_split(w.clone(), Some(&p), Some(point), area, &cfg, Some(stacked));
                }
                None => ws.floating.push(w.clone()),
            }
            self.relayout();
            self.focus(Some(w), SERIAL_COUNTER.next_serial());
            return;
        }
        // Into an unlocked group next to it.
        let target = self.neighbour(&w, dir).filter(|n| self.group_locked(n) == Some(false));
        let Some(target) = target else {
            self.run_action(Action::MoveWindow(dir));
            return;
        };
        let ws = self.wm.workspaces.get_mut(&id).unwrap();
        ws.forget(&w);
        let Some(gi) = ws.group_index(&target) else { return };
        let at = ws.groups[gi].current + 1;
        ws.groups[gi].members.insert(at, w.clone());
        ws.show_member(gi, at);
        ws.history.push(w.clone());
        self.space.unmap_elem(&target);
        self.relayout();
        self.focus(Some(w), SERIAL_COUNTER.next_serial());
    }

    /// The group actions (from `run_action`).
    pub fn group_action(&mut self, action: Action) {
        match action {
            Action::ToggleGroup => self.toggle_group(),
            Action::GroupCycle(delta) => self.group_cycle(delta),
            Action::GroupMoveWindow { forward } => self.group_move_window(forward),
            Action::LockGroup(on) => self.group_lock(on),
            Action::MoveWindowOrGroup(dir) => self.move_window_or_group(dir),
            _ => {}
        }
    }

    /// The tab bar's box above a window drawn at `rect` (logical).
    pub fn groupbar_box(&self, rect: Rect) -> Rect {
        let b = self.config.settings.border_size.max(0) as f64;
        let h = self.config.settings.group.bar.reserved();
        Rect { x: rect.x - b, y: rect.y - b - h, w: rect.w + 2.0 * b, h }
    }

    /// The tab under a point: its window. Shown windows' bars, top first.
    pub fn groupbar_hit(&self, x: f64, y: f64) -> Option<Window> {
        let bar = &self.config.settings.group.bar;
        for window in self.space.elements().rev() {
            if self.groupbar_height(window) <= 0.0 {
                continue;
            }
            let Some(rect) = crate::anim::with_anim(window, |a| *a.rect.goal()) else { continue };
            let bx = self.groupbar_box(rect);
            if x < bx.x || x >= bx.x + bx.w || y < bx.y || y >= bx.y + bx.h {
                continue;
            }
            let id = self.workspace_of(window)?;
            let g = self.wm.workspaces[&id].group_of(window)?;
            let n = g.members.len() as f64;
            let tab = (bx.w - bar.gaps_in * (n - 1.0)) / n;
            let i = (((x - bx.x) / (tab + bar.gaps_in)).floor() as usize).min(g.members.len() - 1);
            return Some(g.members[i].clone());
        }
        None
    }
}
