//! Mouse binds that grab the pointer: `hl.dsp.window.drag()` and
//! `hl.dsp.window.resize()` (Hyprland's `mouse:272` / `mouse:273` with
//! `{ mouse = true }`).
//!
//! A floating window follows the pointer, or resizes from the corner nearest
//! where the drag started. A tiled window moves with the pointer and, let go
//! over another tile, swaps places with it (else it slides back); resizing a
//! tile moves the split ratios (`Dwindle::resize`, Hyprland's smart
//! resizing). While a drag runs the pointer belongs to the compositor: clients
//! get no motion or buttons, and the windows involved follow without animating.

use smithay::{
    desktop::Window,
    input::pointer::{CursorIcon, CursorImageStatus},
    utils::{Logical, Point, SERIAL_COUNTER},
};

use crate::{
    binds::Action,
    dwindle::{Edges, Rect},
    state::State,
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DragKind {
    Move,
    Resize,
}

pub struct Drag {
    pub kind: DragKind,
    pub window: Window,
    /// The button that started it (its release ends it).
    pub button: u32,
    pub floating: bool,
    start_pointer: Point<f64, Logical>,
    last_pointer: Point<f64, Logical>,
    /// The window's rectangle when the drag started.
    start: Rect,
    edges: Edges,
}

impl Drag {
    /// How far the pointer has moved since the drag started.
    pub fn moved(&self) -> (f64, f64) {
        (self.last_pointer.x - self.start_pointer.x, self.last_pointer.y - self.start_pointer.y)
    }
}

impl State {
    /// A mouse bind on a button press: start a drag, or run the bind's
    /// action. True if the press was a bind (not for clients).
    pub fn mouse_bind(&mut self, button: u32, action: Action) -> bool {
        let kind = match action {
            Action::MouseDrag => DragKind::Move,
            Action::MouseResize => DragKind::Resize,
            other => {
                self.run_action(other);
                return true;
            }
        };
        let pos = self.pointer_location;
        let Some(window) = self.window_under(pos) else { return false };
        if self.is_fullscreen(&window) {
            return true;
        }
        let Some(start) = self.goal_rect(&window) else { return false };
        let (cx, cy) = (start.x + start.w / 2.0, start.y + start.h / 2.0);
        let (left, top) = (pos.x < cx, pos.y < cy);
        let edges = Edges { left, top, right: !left, bottom: !top };
        let floating = self.is_floating_here(&window);
        self.focus(Some(window.clone()), SERIAL_COUNTER.next_serial());
        // Above the other windows while it moves.
        self.space.raise_element(&window, false);
        self.cursor_status = CursorImageStatus::Named(match kind {
            DragKind::Move => CursorIcon::Grabbing,
            DragKind::Resize if left == top => CursorIcon::NwseResize,
            DragKind::Resize => CursorIcon::NeswResize,
        });
        tracing::debug!(?kind, floating, "drag starts");
        self.drag = Some(Drag { kind, window, button, floating, start_pointer: pos, last_pointer: pos, start, edges });
        true
    }

    /// The pointer moved during a drag.
    pub fn drag_motion(&mut self, pos: Point<f64, Logical>) {
        let Some(drag) = self.drag.as_mut() else { return };
        let step = (pos.x - drag.last_pointer.x, pos.y - drag.last_pointer.y);
        drag.last_pointer = pos;
        let (dx, dy) = drag.moved();
        let (kind, floating, window, start, edges) = (drag.kind, drag.floating, drag.window.clone(), drag.start, drag.edges);
        match (kind, floating) {
            (DragKind::Move, true) => {
                let loc = ((start.x + dx).round() as i32, (start.y + dy).round() as i32);
                self.space.map_element(window, loc, false);
            }
            // A tile is drawn at its place plus the drag (`State::goal_rect`).
            (DragKind::Move, false) => {}
            (DragKind::Resize, true) => {
                let min = 50.0;
                let (mut x, mut y, mut w, mut h) = (start.x, start.y, start.w, start.h);
                if edges.left {
                    let nw = (w - dx).max(min);
                    x += w - nw;
                    w = nw;
                } else {
                    w = (w + dx).max(min);
                }
                if edges.top {
                    let nh = (h - dy).max(min);
                    y += h - nh;
                    h = nh;
                } else {
                    h = (h + dy).max(min);
                }
                crate::wm::configure(&window, Some((w.round() as i32, h.round() as i32).into()), false, false);
                self.space.map_element(window, (x.round() as i32, y.round() as i32), false);
            }
            (DragKind::Resize, false) => {
                let area = self.work_area();
                let cfg = self.config.settings.dwindle.clone();
                if let Some(ws) = self.wm.workspaces.get_mut(&self.wm.active) {
                    ws.tiled.resize(&window, step, edges, area, &cfg);
                }
                self.relayout();
            }
        }
        self.needs_redraw = true;
    }

    /// The drag's button was released.
    pub fn end_drag(&mut self) {
        let Some(drag) = self.drag.take() else { return };
        tracing::debug!(kind = ?drag.kind, "drag ends");
        if drag.kind == DragKind::Move && !drag.floating {
            // Dropped on another tile: trade places.
            let pos = self.pointer_location;
            let target = self.space.elements().find(|w| {
                **w != drag.window
                    && self.goal_rect(w).is_some_and(|r| pos.x >= r.x && pos.x < r.x + r.w && pos.y >= r.y && pos.y < r.y + r.h)
            });
            if let Some(target) = target.cloned() {
                if let Some(ws) = self.wm.workspaces.get_mut(&self.wm.active) {
                    if ws.tiled.contains(&target) {
                        ws.tiled.swap(&drag.window, &target);
                    }
                }
            }
            // relayout() puts the floating windows back on top.
            self.relayout();
        }
        self.cursor_status = CursorImageStatus::default_named();
        self.needs_redraw = true;
    }

    /// Whether a window is being dragged or resized (it follows without animating).
    pub fn dragging(&self, window: &Window) -> bool {
        self.drag.as_ref().is_some_and(|d| d.window == *window || (d.kind == DragKind::Resize && !d.floating))
    }

    /// A tile being dragged is drawn displaced by the drag.
    pub fn drag_offset(&self, window: &Window) -> Option<(f64, f64)> {
        let d = self.drag.as_ref()?;
        (d.kind == DragKind::Move && !d.floating && d.window == *window).then(|| d.moved())
    }
}
