//! Compositor state and Smithay protocol handlers.
//!
//! The handler structure follows Smithay's `smallvil` example (MIT). Window
//! management (workspaces, tiling, focus) is in `wm.rs`.

use std::{cell::Cell, ffi::OsString, process::Child, sync::Arc, time::Instant};

use smithay::{
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Space, Window,
        WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{CursorIcon, CursorImageStatus, Focus},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{Interest, LoopHandle, Mode as FdMode, PostAction, generic::Generic},
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, Rectangle, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
        cursor_shape::CursorShapeManagerState,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
            with_states,
        },
        output::{OutputHandler, OutputManagerState},
        pointer_constraints::PointerConstraintsHandler,
        selection::{
            SelectionHandler,
            data_device::{DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler, set_data_device_focus},
        },
        shell::xdg::{
            PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
            XdgToplevelSurfaceData,
        },
        shm::{ShmHandler, ShmState},
        socket::ListeningSocketSource,
    },
};

use smithay::backend::renderer::utils::on_commit_buffer_handler;

use crate::{config::Config, wm::Wm};

pub struct State {
    pub config: Config,
    pub wm: Wm,
    /// An exit dispatcher ran: leave at the next frame.
    pub quit_requested: bool,
    pub start_time: Instant,
    pub display_handle: DisplayHandle,
    pub socket_name: OsString,

    pub space: Space<Window>,
    pub popups: PopupManager,
    pub output: Output,
    /// The output in logical points (the window's content size).
    pub output_size: Size<i32, Logical>,
    pub output_scale: f64,

    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: smithay::wayland::shell::wlr_layer::WlrLayerShellState,
    pub shm_state: ShmState,
    /// Kept alive: owns the xdg-output global.
    pub _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub primary_selection_state: smithay::wayland::selection::primary_selection::PrimarySelectionState,
    pub data_control_state: smithay::wayland::selection::ext_data_control::DataControlState,
    pub wlr_data_control_state: smithay::wayland::selection::wlr_data_control::DataControlState,
    pub seat: Seat<State>,

    /// Keeps the cursor-shape global alive.
    pub _cursor_shape_state: CursorShapeManagerState,
    /// Kept alive: owns the wp_presentation global.
    pub _presentation_state: smithay::wayland::presentation::PresentationState,
    /// What the client under the pointer wants as cursor.
    pub cursor_status: CursorImageStatus,
    pub pointer_location: Point<f64, Logical>,
    /// Something changed on screen since the last frame.
    pub needs_redraw: bool,
    /// Every animation leaf's timing, from the config.
    pub timings: crate::anim::Timings,
    /// Windows animating out.
    pub closing: Vec<crate::anim::Closing>,
    /// Counts `animate` calls.
    pub anim_frame: u64,
    /// Hyprland-compatible IPC sockets.
    pub ipc: crate::ipc::Ipc,
    /// A mouse-bind move or resize in progress.
    pub drag: Option<crate::mouse::Drag>,
    /// Buttons whose press was a bind: their release isn't sent either.
    pub bound_buttons: std::collections::HashSet<u32>,
    /// A trackpad workspace swipe in progress.
    pub swipe: crate::gesture::Swipe,
    pub children: Vec<Child>,
}

impl State {
    pub fn new(display: Display<State>, loop_handle: &LoopHandle<'static, State>, config: Config) -> State {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<State>(&dh);
        // Only fullscreen: a tiling compositor has no minimize or maximize, and
        // clients (GTK) hide those buttons when they aren't advertised.
        let xdg_shell_state = XdgShellState::new_with_capabilities::<State>(
            &dh,
            [smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::WmCapabilities::Fullscreen],
        );
        let shm_state = ShmState::new::<State>(&dh, vec![]);
        let layer_shell_state = smithay::wayland::shell::wlr_layer::WlrLayerShellState::new::<State>(&dh);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&dh);
        let data_device_state = DataDeviceState::new::<State>(&dh);
        // Middle-click paste, and clipboard managers / wl-clipboard (both
        // data-control flavours; any client may use them, as in Hyprland).
        let primary_selection_state =
            smithay::wayland::selection::primary_selection::PrimarySelectionState::new::<State>(&dh);
        let data_control_state = smithay::wayland::selection::ext_data_control::DataControlState::new::<State, _>(
            &dh,
            Some(&primary_selection_state),
            |_| true,
        );
        let wlr_data_control_state = smithay::wayland::selection::wlr_data_control::DataControlState::new::<State, _>(
            &dh,
            Some(&primary_selection_state),
            |_| true,
        );
        let cursor_shape_state = CursorShapeManagerState::new::<State>(&dh);
        let presentation_state =
            smithay::wayland::presentation::PresentationState::new::<State>(&dh, crate::app::CLOCK_UPTIME_RAW);

        let mut seat_state = SeatState::new();
        let mut seat: Seat<State> = seat_state.new_wl_seat(&dh, "seat0");
        // libxkbcommon's defaults (evdev rules, pc105, us). Repeat is the client's job.
        seat.add_keyboard(Default::default(), 400, 30).expect("keymap");
        seat.add_pointer();

        let output = Output::new(
            "darwin-0".into(),
            PhysicalProperties { size: (0, 0).into(), subpixel: Subpixel::Unknown, make: "Apple".into(), model: "window".into(), serial_number: String::new() },
        );
        let _global = output.create_global::<State>(&dh);

        let listening = ListeningSocketSource::new_auto().expect("creating the Wayland socket");
        let socket_name = listening.socket_name().to_os_string();
        loop_handle
            .insert_source(listening, |stream, _, state: &mut State| {
                if let Err(err) = state.display_handle.insert_client(stream, Arc::new(ClientState::default())) {
                    tracing::warn!("rejecting client: {err}");
                }
            })
            .expect("socket source");
        loop_handle
            .insert_source(Generic::new(display, Interest::READ, FdMode::Level), |_, display, state| {
                // SAFETY: the display is never dropped while the source lives.
                unsafe { display.get_mut().dispatch_clients(state).expect("dispatching clients") };
                Ok(PostAction::Continue)
            })
            .expect("display source");

        State {
            timings: crate::anim::Timings::new(&config.settings),
            config,
            wm: Wm::default(),
            quit_requested: false,
            start_time: Instant::now(),
            display_handle: dh,
            socket_name,
            space: Space::default(),
            popups: PopupManager::default(),
            output,
            output_size: (0, 0).into(),
            output_scale: 1.0,
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            shm_state,
            _output_manager_state: output_manager_state,
            seat_state,
            data_device_state,
            primary_selection_state,
            data_control_state,
            wlr_data_control_state,
            seat,
            _cursor_shape_state: cursor_shape_state,
            _presentation_state: presentation_state,
            cursor_status: CursorImageStatus::Named(CursorIcon::Default),
            pointer_location: (0.0, 0.0).into(),
            needs_redraw: true,
            closing: Vec::new(),
            anim_frame: 0,
            ipc: crate::ipc::Ipc::new(loop_handle),
            drag: None,
            bound_buttons: Default::default(),
            swipe: Default::default(),
            children: Vec::new(),
        }
    }

    /// The window's content size or backing scale changed.
    pub fn set_output_size(&mut self, size: Size<i32, Logical>, scale: f64, refresh_mhz: i32) {
        self.output_size = size;
        self.output_scale = scale;
        let mode = Mode { size: size.to_f64().to_physical(scale).to_i32_round(), refresh: refresh_mhz };
        self.output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            Some(Scale::Integer(scale.round().max(1.0) as i32)),
            Some((0, 0).into()),
        );
        self.output.set_preferred(mode);
        self.space.map_output(&self.output, (0, 0));
        smithay::desktop::layer_map_for_output(&self.output).arrange();
        self.relayout();
    }

    /// The surface under a point, in drawing order: overlay and top layer
    /// surfaces, windows, then bottom and background ones.
    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        use smithay::wayland::shell::wlr_layer::Layer;
        let layer = |layers: &[Layer]| self.layer_surface_under(layers, pos).map(|(_, s, p)| (s, p));
        layer(self.upper_layers())
            .or_else(|| {
                self.space.element_under(pos).and_then(|(window, location)| {
                    window
                        .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                        .map(|(s, p)| (s, (p + location).to_f64()))
                })
            })
            .or_else(|| layer(&[Layer::Bottom, Layer::Background]))
    }

    /// The window under a point, unless a top or overlay layer surface covers it.
    pub fn window_under(&self, pos: Point<f64, Logical>) -> Option<Window> {
        if self.layer_surface_under(self.upper_layers(), pos).is_some() {
            return None;
        }
        self.space.element_under(pos).map(|(w, _)| w.clone())
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(window) = self.find_window(&root) else { return };
        let Some(window_loc) = self.space.element_location(&window) else { return };
        // The output in the popup parent's coordinates.
        let mut target = Rectangle::from_size(self.output_size);
        target.loc -= window_loc;
        target.loc -= get_popup_toplevel_coords(&PopupKind::Xdg(popup.clone()));
        popup.with_pending_state(|state| {
            state.geometry = state.positioner.get_unconstrained_geometry(target);
        });
    }
}

#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

// ---- wl_compositor, wl_shm ----

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().expect("client data").compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        self.needs_redraw = true;
        if self.layer_commit(surface) {
            self.popups.commit(surface);
            return;
        }

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.find_window(&root) {
                window.on_commit();
                if &root == surface {
                    tracing::trace!(
                        geometry = ?window.geometry(),
                        bbox = ?window.bbox(),
                        location = ?self.space.element_location(&window),
                        "commit"
                    );
                }
            }
        }

        // A toplevel's first commit: tile or float it, then send the initial configure.
        let pending = self.wm.pending.iter().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface)).cloned();
        if let Some(window) = pending {
            let initial_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            if !initial_sent {
                self.manage(window.clone());
                window.toplevel().unwrap().send_configure();
            }
        } else if let Some(window) = self.find_window(surface).filter(|w| self.is_floating_here(w)) {
            // A floating window is centred (or put where a `move` rule says)
            // once its size is known, if it's on screen.
            window.user_data().insert_if_missing(|| Cell::new(false));
            let placed = window.user_data().get::<Cell<bool>>().unwrap();
            let size = window.geometry().size;
            let on_screen = self.space.element_location(&window).is_some();
            if !placed.get() && on_screen && size.w > 0 && size.h > 0 {
                placed.set(true);
                let moved = window
                    .user_data()
                    .get::<crate::rules::Placement>()
                    .and_then(|p| p.move_to.clone())
                    .and_then(|m| crate::rules::eval_pair(&m, &self.rule_vars((size.w as f64, size.h as f64))));
                let loc = match moved {
                    Some((x, y)) => (x.round() as i32, y.round() as i32),
                    None => ((self.output_size.w - size.w) / 2, (self.output_size.h - size.h) / 2),
                };
                self.space.map_element(window.clone(), loc, true);
            }
        }

        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(xdg)) = self.popups.find_popup(surface) {
            if !xdg.is_initial_configure_sent() {
                xdg.send_configure().expect("initial popup configure");
            }
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

// ---- xdg_shell ----

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    /// Managed at its first commit, once its parent and size hints are known.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        self.wm.pending.push(Window::new_wayland_window(surface));
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.find_window(surface.wl_surface()) {
            self.unmanage(&window);
        }
    }

    /// A client asked for fullscreen (e.g. F11): the same as the fullscreen bind.
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        if let Some(window) = self.find_window(surface.wl_surface()) {
            // `suppress_event = "fullscreen"`: ignored.
            if self.rule_effects(&window).suppress.iter().any(|e| e == "fullscreen") {
                return;
            }
            self.set_fullscreen(&window, true);
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.find_window(surface.wl_surface()) {
            self.set_fullscreen(&window, false);
        }
    }

    fn new_popup(&mut self, surface: PopupSurface, _positioner: PositionerState) {
        self.unconstrain_popup(&surface);
        if let Err(err) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::warn!("popup: {err}");
        }
    }

    fn reposition_request(&mut self, surface: PopupSurface, positioner: PositionerState, token: u32) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        self.unconstrain_popup(&surface);
        surface.send_repositioned(token);
    }

    fn grab(&mut self, surface: PopupSurface, seat: wl_seat::WlSeat, serial: Serial) {
        let Some(seat) = Seat::<State>::from_resource(&seat) else { return };
        let kind = PopupKind::Xdg(surface);
        let Ok(root) = find_popup_root_surface(&kind) else { return };
        let is_layer = smithay::desktop::layer_map_for_output(&self.output)
            .layer_for_surface(&root, WindowSurfaceType::TOPLEVEL)
            .is_some();
        if self.find_window(&root).is_none() && !is_layer {
            return;
        }
        let Ok(mut grab) = self.popups.grab_popup(root, kind, &seat, serial) else { return };
        if let Some(keyboard) = seat.get_keyboard() {
            if keyboard.is_grabbed()
                && !(keyboard.has_grab(serial) || keyboard.has_grab(grab.previous_serial().unwrap_or(serial)))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            keyboard.set_focus(self, grab.current_grab(), serial);
            keyboard.set_grab(self, PopupKeyboardGrab::new(&grab), serial);
        }
        if let Some(pointer) = seat.get_pointer() {
            if pointer.is_grabbed()
                && !(pointer.has_grab(serial) || pointer.has_grab(grab.previous_serial().unwrap_or_else(|| grab.serial())))
            {
                grab.ungrab(PopupUngrabStrategy::All);
                return;
            }
            pointer.set_grab(self, PopupPointerGrab::new(&grab), serial, Focus::Keep);
        }
    }
}

// ---- wl_seat ----

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<State> {
        &mut self.seat_state
    }

    /// Applied as an `NSCursor` on the next frame (app.rs, cursor.rs).
    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor_status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client.clone());
        smithay::wayland::selection::primary_selection::set_primary_focus(dh, seat, client);
    }
}

/// Required by the cursor-shape protocol (shapes can be set for tablet tools too).
impl smithay::input::tablet::TabletSeatHandler for State {
    type ToolFocus = WlSurface;
}

/// Required for `WlSurface` as pointer focus; no constraints are offered yet.
impl PointerConstraintsHandler for State {}

// ---- data device (clipboard and drag and drop between clients) ----

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

impl smithay::wayland::selection::primary_selection::PrimarySelectionHandler for State {
    fn primary_selection_state(&mut self) -> &mut smithay::wayland::selection::primary_selection::PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

impl smithay::wayland::selection::ext_data_control::DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut smithay::wayland::selection::ext_data_control::DataControlState {
        &mut self.data_control_state
    }
}

impl smithay::wayland::selection::wlr_data_control::DataControlHandler for State {
    fn data_control_state(&mut self) -> &mut smithay::wayland::selection::wlr_data_control::DataControlState {
        &mut self.wlr_data_control_state
    }
}

impl DndGrabHandler for State {}

impl WaylandDndGrabHandler for State {
    fn dnd_requested<S: Source>(
        &mut self,
        source: S,
        _icon: Option<WlSurface>,
        seat: Seat<Self>,
        serial: Serial,
        type_: GrabType,
    ) {
        match type_ {
            GrabType::Pointer => {
                let Some(pointer) = seat.get_pointer() else { return source.cancel() };
                let Some(start_data) = pointer.grab_start_data() else { return source.cancel() };
                let grab = DnDGrab::new_pointer(&self.display_handle, start_data, source, seat);
                pointer.set_grab(self, grab, serial, Focus::Keep);
            }
            GrabType::Touch => source.cancel(),
        }
    }
}

// ---- wp_presentation ----

impl State {
    /// Takes the presentation feedback of every surface just drawn: windows
    /// on screen and their popups, layer surfaces, the cursor.
    pub fn take_presentation_feedback(&self) -> Vec<smithay::desktop::utils::SurfacePresentationFeedback> {
        use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
        use smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind;
        let mut out = Vec::new();
        let mut take = |surface: &WlSurface| {
            with_surface_tree_downward(
                surface,
                (),
                |_, _, &()| TraversalAction::DoChildren(()),
                |_, states, &()| {
                    out.extend(smithay::desktop::utils::SurfacePresentationFeedback::from_states(states, Kind::empty()));
                },
                |_, _, &()| true,
            );
            for (popup, _) in smithay::desktop::PopupManager::popups_for_surface(surface) {
                with_surface_tree_downward(
                    popup.wl_surface(),
                    (),
                    |_, _, &()| TraversalAction::DoChildren(()),
                    |_, states, &()| {
                        out.extend(smithay::desktop::utils::SurfacePresentationFeedback::from_states(states, Kind::empty()));
                    },
                    |_, _, &()| true,
                );
            }
        };
        for window in self.space.elements() {
            if let Some(surface) = smithay::wayland::seat::WaylandFocus::wl_surface(window) {
                take(&surface);
            }
        }
        for layer in smithay::desktop::layer_map_for_output(&self.output).layers() {
            take(layer.wl_surface());
        }
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.cursor_status {
            take(surface);
        }
        out
    }
}

// ---- wl_output, xdg_output ----

impl OutputHandler for State {}

smithay::delegate_dispatch2!(State);
