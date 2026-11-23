//! wlr-layer-shell: bars, launchers, notifications and wallpapers.
//!
//! Layer surfaces live in Smithay's `LayerMap` for the one output, which
//! places them by anchor and margin and keeps the zone left over by
//! exclusive surfaces (a bar). Tiling works inside that zone. Drawing and
//! input go background, bottom, the workspace, top, overlay; a fullscreen
//! window covers the top layer, as in Hyprland.
//!
//! Keyboard: a top or overlay surface asking for `exclusive` interactivity
//! (a launcher) takes the keyboard while it's mapped; `on_demand` ones get it
//! when clicked. Focus goes back to the workspace's window afterwards.

use smithay::{
    desktop::{LayerSurface, PopupKind, WindowSurfaceType, layer_map_for_output},
    reexports::wayland_server::{
        Resource,
        protocol::{wl_output::WlOutput, wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER},
    wayland::{
        compositor::with_states,
        shell::{
            wlr_layer::{
                KeyboardInteractivity, Layer, LayerSurface as WlrLayerSurface, LayerSurfaceData,
                WlrLayerShellHandler, WlrLayerShellState,
            },
            xdg::PopupSurface,
        },
    },
};

use crate::state::State;

impl WlrLayerShellHandler for State {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }

    /// One output: a requested `wl_output` is always ours.
    fn new_layer_surface(&mut self, surface: WlrLayerSurface, _output: Option<WlOutput>, layer: Layer, namespace: String) {
        tracing::debug!(?layer, namespace, "new layer surface");
        let mut map = layer_map_for_output(&self.output);
        if let Err(err) = map.map_layer(&LayerSurface::new(surface, namespace)) {
            tracing::warn!("mapping a layer surface: {err:?}");
        }
    }

    fn new_popup(&mut self, _parent: WlrLayerSurface, popup: PopupSurface) {
        self.unconstrain_layer_popup(&popup);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(popup)) {
            tracing::warn!("tracking a layer popup: {err}");
        }
    }

    fn layer_destroyed(&mut self, surface: WlrLayerSurface) {
        let changed = {
            let mut map = layer_map_for_output(&self.output);
            let layer = map.layers().find(|l| l.layer_surface() == &surface).cloned();
            match layer {
                Some(layer) => {
                    let before = map.non_exclusive_zone();
                    map.unmap_layer(&layer);
                    map.non_exclusive_zone() != before
                }
                None => false,
            }
        };
        if changed {
            self.relayout();
        }
        self.update_layer_focus();
        self.needs_redraw = true;
    }
}

impl State {
    /// A commit on a layer surface: place it, send its first configure, and
    /// relayout if the space it reserves changed. False if it isn't one.
    pub fn layer_commit(&mut self, surface: &WlSurface) -> bool {
        let zone_changed = {
            let mut map = layer_map_for_output(&self.output);
            let Some(layer) = map.layer_for_surface(surface, WindowSurfaceType::TOPLEVEL).cloned() else {
                return false;
            };
            let initial_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<LayerSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            let before = map.non_exclusive_zone();
            map.arrange();
            if !initial_sent {
                layer.layer_surface().send_configure();
            }
            map.non_exclusive_zone() != before
        };
        if zone_changed {
            self.relayout();
        }
        self.update_layer_focus();
        true
    }

    /// Mapped layer surfaces on `layers`, bottom to top, with their surface
    /// origin (logical).
    pub fn layers_on(&self, layers: &[Layer]) -> Vec<(LayerSurface, Point<i32, Logical>)> {
        let map = layer_map_for_output(&self.output);
        layers
            .iter()
            .flat_map(|layer| map.layers_on(*layer))
            .filter_map(|l| map.layer_geometry(l).map(|geo| (l.clone(), geo.loc - l.geometry().loc)))
            .collect()
    }

    /// Whether the active workspace shows a fullscreen window (which covers
    /// the top layer).
    pub fn fullscreen_active(&self) -> bool {
        self.wm.workspaces.get(&self.wm.active).is_some_and(|ws| ws.fullscreen.is_some())
    }

    /// The layers drawn above windows: top (unless a fullscreen window
    /// covers it) and overlay.
    pub fn upper_layers(&self) -> &'static [Layer] {
        if self.fullscreen_active() { &[Layer::Overlay] } else { &[Layer::Top, Layer::Overlay] }
    }

    /// The layer surface (or one of its popups) under a point on `layers`,
    /// topmost first: the surface and its origin.
    pub fn layer_surface_under(
        &self,
        layers: &[Layer],
        pos: Point<f64, Logical>,
    ) -> Option<(LayerSurface, WlSurface, Point<f64, Logical>)> {
        self.layers_on(layers).into_iter().rev().find_map(|(layer, origin)| {
            let origin = origin.to_f64();
            layer
                .surface_under(pos - origin, WindowSurfaceType::ALL)
                .map(|(s, p)| (layer.clone(), s, p.to_f64() + origin))
        })
    }

    /// The top or overlay surface holding the keyboard exclusively, if any.
    fn exclusive_layer(&self) -> Option<LayerSurface> {
        let map = layer_map_for_output(&self.output);
        map.layers()
            .rev()
            .filter(|l| matches!(l.layer(), Layer::Top | Layer::Overlay))
            .find(|l| l.cached_state().keyboard_interactivity == KeyboardInteractivity::Exclusive)
            .cloned()
    }

    /// Give the keyboard to an exclusive layer surface, or take it back
    /// from a layer surface that no longer wants it (or went away).
    pub fn update_layer_focus(&mut self) {
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        if let Some(layer) = self.exclusive_layer() {
            let surface = layer.wl_surface().clone();
            if keyboard.current_focus().as_ref() != Some(&surface) {
                keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
            }
            return;
        }
        let focus = keyboard.current_focus();
        let keep = match &focus {
            None => false,
            Some(s) if !s.is_alive() => false,
            Some(s) => {
                let map = layer_map_for_output(&self.output);
                // A window keeps it; a layer surface only while it still wants it.
                map.layer_for_surface(s, WindowSurfaceType::TOPLEVEL).is_none_or(|l| l.can_receive_keyboard_focus())
            }
        };
        if !keep {
            let next = self.wm.workspaces.get(&self.wm.active).and_then(|ws| ws.last_focused());
            if next.is_some() || focus.is_some() {
                self.focus(next, SERIAL_COUNTER.next_serial());
            }
        }
    }

    /// A click on a layer surface that takes the keyboard on demand.
    pub fn focus_layer_on_click(&mut self, layer: &LayerSurface) {
        if self.exclusive_layer().is_some() || !layer.can_receive_keyboard_focus() {
            return;
        }
        let Some(keyboard) = self.seat.get_keyboard() else { return };
        let surface = layer.wl_surface().clone();
        if keyboard.current_focus().as_ref() != Some(&surface) {
            keyboard.set_focus(self, Some(surface), SERIAL_COUNTER.next_serial());
        }
    }

    /// While an exclusive layer surface is up, windows can't take the keyboard.
    pub fn keyboard_locked_to_layer(&self) -> bool {
        self.exclusive_layer().is_some()
    }

    /// Keep a layer surface's popup on the output.
    fn unconstrain_layer_popup(&self, popup: &PopupSurface) {
        let Some(parent) = popup.get_parent_surface() else { return };
        let map = layer_map_for_output(&self.output);
        let Some(layer) = map.layer_for_surface(&parent, WindowSurfaceType::TOPLEVEL) else { return };
        let Some(geo) = map.layer_geometry(layer) else { return };
        // The output in the layer surface's coordinates.
        let mut target = Rectangle::from_size(self.output_size);
        target.loc -= geo.loc - layer.geometry().loc;
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}
