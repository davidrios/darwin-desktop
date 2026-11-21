//! Compositor state and Smithay protocol handlers.
//!
//! The handler structure follows Smithay's `smallvil` example (MIT). Window
//! policy for M1: a toplevel without a parent fills the output (maximized), a
//! dialog keeps its own size and is centred, the newest window gets focus.

use std::{ffi::OsString, process::Child, sync::Arc, time::Instant};

use smithay::{
    desktop::{
        PopupKeyboardGrab, PopupKind, PopupManager, PopupPointerGrab, PopupUngrabStrategy, Space, Window,
        WindowSurfaceType, find_popup_root_surface, get_popup_toplevel_coords,
    },
    input::{
        Seat, SeatHandler, SeatState,
        dnd::{DnDGrab, DndGrabHandler, GrabType, Source},
        pointer::{CursorImageStatus, Focus},
    },
    output::{Mode, Output, PhysicalProperties, Scale, Subpixel},
    reexports::{
        calloop::{Interest, LoopHandle, Mode as FdMode, PostAction, generic::Generic},
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::{
            Client, Display, DisplayHandle, Resource,
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_buffer, wl_seat, wl_surface::WlSurface},
        },
    },
    utils::{Logical, Point, Rectangle, SERIAL_COUNTER, Serial, Size, Transform},
    wayland::{
        buffer::BufferHandler,
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

pub struct State {
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
    pub shm_state: ShmState,
    /// Kept alive: owns the xdg-output global.
    pub _output_manager_state: OutputManagerState,
    pub seat_state: SeatState<State>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<State>,

    pub pointer_location: Point<f64, Logical>,
    /// Something changed on screen since the last frame.
    pub needs_redraw: bool,
    pub children: Vec<Child>,
}

impl State {
    pub fn new(display: Display<State>, loop_handle: &LoopHandle<'static, State>) -> State {
        let dh = display.handle();

        let compositor_state = CompositorState::new::<State>(&dh);
        let xdg_shell_state = XdgShellState::new::<State>(&dh);
        let shm_state = ShmState::new::<State>(&dh, vec![]);
        let output_manager_state = OutputManagerState::new_with_xdg_output::<State>(&dh);
        let data_device_state = DataDeviceState::new::<State>(&dh);

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
            shm_state,
            _output_manager_state: output_manager_state,
            seat_state,
            data_device_state,
            seat,
            pointer_location: (0.0, 0.0).into(),
            needs_redraw: true,
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
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            self.place(&window);
        }
        self.needs_redraw = true;
    }

    /// M1 policy: parentless toplevels fill the output; dialogs are centred.
    fn place(&mut self, window: &Window) {
        let Some(toplevel) = window.toplevel() else { return };
        if toplevel.parent().is_none() {
            toplevel.with_pending_state(|s| {
                s.size = Some(self.output_size);
                s.states.set(xdg_toplevel::State::Maximized);
            });
            self.space.map_element(window.clone(), (0, 0), false);
        } else {
            let size = window.geometry().size;
            let loc = ((self.output_size.w - size.w) / 2, (self.output_size.h - size.h) / 2);
            self.space.map_element(window.clone(), loc, false);
        }
        if toplevel.is_initial_configure_sent() {
            toplevel.send_pending_configure();
        }
    }

    pub fn surface_under(&self, pos: Point<f64, Logical>) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.space.element_under(pos).and_then(|(window, location)| {
            window
                .surface_under(pos - location.to_f64(), WindowSurfaceType::ALL)
                .map(|(s, p)| (s, (p + location).to_f64()))
        })
    }

    pub fn window_for_surface(&self, surface: &WlSurface) -> Option<Window> {
        self.space.elements().find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface)).cloned()
    }

    /// Raise, activate and give keyboard focus to a window.
    pub fn focus(&mut self, window: Option<Window>, serial: Serial) {
        if let Some(window) = &window {
            self.space.raise_element(window, true);
        }
        for w in self.space.elements() {
            let active = window.as_ref() == Some(w);
            w.set_activated(active);
            if let Some(t) = w.toplevel() {
                if t.is_initial_configure_sent() {
                    t.send_pending_configure();
                }
            }
        }
        let surface = window.and_then(|w| w.toplevel().map(|t| t.wl_surface().clone()));
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface, serial);
        }
        self.needs_redraw = true;
    }

    fn unconstrain_popup(&self, popup: &PopupSurface) {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else { return };
        let Some(window) = self.window_for_surface(&root) else { return };
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

        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            if let Some(window) = self.window_for_surface(&root) {
                window.on_commit();
            }
        }

        // A toplevel's first commit: decide its size, then send the initial configure.
        if let Some(window) = self.window_for_surface(surface) {
            let initial_sent = with_states(surface, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|d| d.lock().unwrap().initial_configure_sent)
                    .unwrap_or(true)
            });
            if !initial_sent {
                self.place(&window);
                window.toplevel().unwrap().send_configure();
            } else if window.toplevel().is_some_and(|t| t.parent().is_some()) {
                // Dialogs: re-centre once their size is known or changes.
                self.place(&window);
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

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        self.space.map_element(window.clone(), (0, 0), false);
        self.focus(Some(window), SERIAL_COUNTER.next_serial());
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_for_surface(surface.wl_surface()) {
            self.space.unmap_elem(&window);
        }
        let top = self.space.elements().last().cloned();
        self.focus(top, SERIAL_COUNTER.next_serial());
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
        if self.window_for_surface(&root).is_none() {
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

    /// M1 keeps the macOS arrow cursor; client cursor surfaces are ignored.
    fn cursor_image(&mut self, _seat: &Seat<Self>, _image: CursorImageStatus) {}

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let dh = &self.display_handle;
        let client = focused.and_then(|s| dh.get_client(s.id()).ok());
        set_data_device_focus(dh, seat, client);
    }
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

// ---- wl_output, xdg_output ----

impl OutputHandler for State {}

smithay::delegate_dispatch2!(State);
