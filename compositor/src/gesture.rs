//! Trackpad gestures: `hl.gesture({ fingers = 3, direction = "horizontal",
//! action = "workspace" })`, Hyprland's workspace swipe.
//!
//! The view asks AppKit for the trackpad's raw touches (`NSTouch`, indirect
//! touches: public API, normalized finger positions on the pad). With the
//! configured number of fingers down, the centroid's movement along the
//! gesture's axis drags the active workspace and its neighbour (one number up
//! or down) with the fingers, drawn through the workspace transition. On
//! lift it finishes the switch if the swipe went past
//! `workspace_swipe_cancel_ratio` or was fast, and slides back otherwise.
//!
//! macOS has its own three- and four-finger swipes (Spaces, Mission
//! Control); they still happen unless turned off or moved to the other
//! finger count in System Settings > Trackpad.

use std::time::Instant;

use objc2_app_kit::{NSEvent, NSTouchPhase};

use crate::{
    anim::{Animated, Transition},
    state::State,
};

/// Trackpad points (about 1/72 inch) to Hyprland's swipe units: a full
/// workspace (300 units by default) is a swipe of about 3.5 cm.
const UNITS_PER_POINT: f64 = 3.0;
/// Movement before the swipe picks its axis, in trackpad points.
const DEAD_ZONE: f64 = 4.0;

#[derive(Default)]
pub struct Swipe {
    track: Option<Track>,
}

struct Track {
    fingers: usize,
    start: (f64, f64),
    last: (f64, f64),
    last_time: Option<Instant>,
    /// Decided after the dead zone: true horizontal. `active` if it matches the gesture.
    axis: Option<bool>,
    active: bool,
    horizontal: bool,
    progress: f64,
    /// Swipe units per 10 ms, smoothed.
    speed: f64,
}

impl Swipe {
    /// A workspace swipe is following the fingers.
    pub fn active(&self) -> bool {
        self.track.as_ref().is_some_and(|t| t.active)
    }
}

/// The fingers on the trackpad changed: count them and find their centre.
pub fn touches(state: &mut State, event: &NSEvent) {
    let set = event.touchesMatchingPhase_inView(NSTouchPhase::Touching, None);
    let mut n = 0usize;
    let (mut x, mut y) = (0.0, 0.0);
    for touch in set.iter() {
        let p = touch.normalizedPosition();
        let size = touch.deviceSize();
        // Normalized positions start at the bottom left; points, y down.
        x += p.x * size.width;
        y += (1.0 - p.y) * size.height;
        n += 1;
    }
    let centre = if n > 0 { (x / n as f64, y / n as f64) } else { (0.0, 0.0) };
    state.touch_frame(n, centre);
}

impl State {
    fn touch_frame(&mut self, fingers: usize, centre: (f64, f64)) {
        let gesture = self
            .config
            .settings
            .gestures
            .iter()
            .find(|g| g.action == "workspace" && g.fingers as usize == fingers)
            .cloned();
        if let Some(track) = &self.swipe.track {
            if track.fingers != fingers {
                let track = self.swipe.track.take().unwrap();
                if track.active {
                    self.swipe_end(track.progress, track.speed, track.horizontal);
                }
            }
        }
        let Some(gesture) = gesture else { return };
        let track = self.swipe.track.get_or_insert(Track {
            fingers,
            start: centre,
            last: centre,
            last_time: None,
            axis: None,
            active: false,
            horizontal: true,
            progress: 0.0,
            speed: 0.0,
        });
        let (dx, dy) = (centre.0 - track.start.0, centre.1 - track.start.1);
        if track.axis.is_none() {
            if dx.hypot(dy) < DEAD_ZONE {
                return;
            }
            let horizontal = dx.abs() >= dy.abs();
            track.axis = Some(horizontal);
            let wants_horizontal = matches!(gesture.direction.as_str(), "horizontal" | "left" | "right");
            track.active = horizontal == wants_horizontal;
            track.horizontal = horizontal;
            // Start from here, not from where the fingers landed.
            track.start = centre;
            track.last = centre;
        }
        if !track.active {
            return;
        }
        let along = |p: (f64, f64)| if track.horizontal { p.0 } else { p.1 };
        let sign = if self.config.settings.swipe.invert { -1.0 } else { 1.0 };
        let now = Instant::now();
        let step = sign * (along(centre) - along(track.last)) * UNITS_PER_POINT;
        if let Some(t) = track.last_time {
            let dt = now.duration_since(t).as_secs_f64() * 100.0; // in 10 ms
            if dt > 0.0 {
                track.speed = 0.6 * track.speed + 0.4 * (step / dt);
            }
        }
        track.last_time = Some(now);
        track.last = centre;
        let total = sign * (along(centre) - along(track.start)) * UNITS_PER_POINT;
        let progress = total / self.config.settings.swipe.distance;
        let horizontal = track.horizontal;
        let shown = self.swipe_update(progress, horizontal);
        if let Some(track) = self.swipe.track.as_mut() {
            track.progress = shown;
        }
    }

    /// The neighbour a swipe at `progress` shows, if it may: the next
    /// workspace for positive progress, the previous for negative.
    fn swipe_neighbour(&self, progress: f64) -> Option<i32> {
        let next = self.wm.active + if progress >= 0.0 { 1 } else { -1 };
        let exists = self.wm.workspaces.get(&next).is_some_and(|ws| !ws.is_empty());
        (next >= 1 && (exists || self.config.settings.swipe.create_new)).then_some(next)
    }

    /// The distance a workspace slides along the swipe's axis.
    fn swipe_span(&self, horizontal: bool) -> f64 {
        let size = if horizontal { self.output_size.w } else { self.output_size.h };
        size as f64 + self.config.settings.workspace_gap
    }

    /// Draw the swipe at `progress` (fraction of a workspace); returns the
    /// progress actually shown (none towards a workspace that can't be).
    fn swipe_update(&mut self, progress: f64, horizontal: bool) -> f64 {
        let neighbour = self.swipe_neighbour(progress);
        let progress = if neighbour.is_some() { progress.clamp(-1.0, 1.0) } else { 0.0 };
        let span = self.swipe_span(horizontal);
        let along = |d: f64| if horizontal { [d, 0.0] } else { [0.0, d] };
        let active_offset = -progress * span;
        let neighbour_offset = active_offset + progress.signum() * span;
        self.wm.transition = Some(Transition {
            // No neighbour: a workspace number that doesn't exist draws nothing.
            from: neighbour.unwrap_or(0),
            out_alpha: Animated::new(1.0),
            out_offset: Animated::new(along(neighbour_offset)),
            in_alpha: Animated::new(1.0),
            in_offset: Animated::new(along(active_offset)),
        });
        self.needs_redraw = true;
        progress
    }

    /// Fingers lifted: finish the switch or slide back.
    fn swipe_end(&mut self, progress: f64, speed: f64, horizontal: bool) {
        let cfg = &self.config.settings.swipe;
        let fast = speed.abs() >= cfg.min_speed_to_force && speed.signum() == progress.signum();
        let commit = progress != 0.0 && (progress.abs() >= cfg.cancel_ratio || fast);
        let span = self.swipe_span(horizontal);
        let along = |d: f64| if horizontal { [d, 0.0] } else { [0.0, d] };
        let sign = progress.signum();
        let active_offset = -progress * span;
        let neighbour_offset = active_offset + sign * span;
        let now = Instant::now();
        let (t_in, t_out) = (self.timings.get("workspacesIn").clone(), self.timings.get("workspacesOut").clone());
        let neighbour = self.swipe_neighbour(progress);
        let mut out_offset = Animated::new(along(0.0));
        let mut in_offset = Animated::new(along(0.0));
        let from = match neighbour {
            Some(next) if commit => {
                let old = self.wm.active;
                self.switch_workspace(next);
                out_offset.animate_from(along(active_offset), along(-sign * span), &t_out, now);
                in_offset.animate_from(along(neighbour_offset), along(0.0), &t_in, now);
                old
            }
            _ => {
                out_offset.animate_from(along(neighbour_offset), along(sign * span), &t_out, now);
                in_offset.animate_from(along(active_offset), along(0.0), &t_in, now);
                neighbour.unwrap_or(0)
            }
        };
        let tr = Transition { from, out_alpha: Animated::new(1.0), out_offset, in_alpha: Animated::new(1.0), in_offset };
        self.wm.transition = tr.active().then_some(tr);
        self.needs_redraw = true;
    }
}
