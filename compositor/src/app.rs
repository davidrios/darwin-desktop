//! The AppKit side: the window, its Metal-backed view, the run loop.
//!
//! Event loop (outline section 6, item 4): AppKit owns the main thread. The
//! compositor's calloop loop (Wayland sockets) is driven from the same run
//! loop: its kqueue fd is wrapped in a `CFFileDescriptor`, and when it turns
//! readable the loop is dispatched without blocking. Frames come from the
//! view's `CADisplayLink` at the display's refresh rate. The link pauses
//! after a few frames with nothing to draw and resumes on any event (a
//! client request, input, a window notification), so an idle session
//! doesn't wake up at the refresh rate. Everything runs on the main thread,
//! so the state lives in a thread-local.
//!
//! Keys come through a local event monitor rather than `keyDown:`: AppKit
//! sends no `keyUp:` for keys released while Command is held, and the monitor
//! sees keys before menus' key equivalents (as mitsuami's keyboard grab does).

use std::{
    cell::RefCell,
    ffi::c_void,
    process::Command,
    ptr::{self, NonNull},
    time::Duration,
};

use block2::RcBlock;
use objc2::{
    DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSCursor, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSEventType, NSResponder,
    NSScreen, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_foundation::{
    CFFileDescriptor, CFOptionFlags, CFRetained, CFRunLoop, CFRunLoopSource, CGSize, kCFFileDescriptorReadCallBack,
    kCFRunLoopCommonModes,
};
use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString};
use objc2_quartz_core::CADisplayLink;
use smithay::{
    backend::input::InputTime,
    desktop::utils::SurfacePresentationFeedback,
    reexports::{
        calloop::EventLoop, wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::Display,
    },
    utils::{Logical, Size},
    wayland::presentation::Refresh,
};

use crate::{
    Options,
    capture::Capture,
    cursor::{self, CursorKey},
    input::{self, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Keys, event_time},
    layout::{self, MacLayout},
    render::Renderer,
    state::State,
};

pub struct App {
    mtm: MainThreadMarker,
    capture: Capture,
    event_loop: EventLoop<'static, State>,
    state: State,
    renderer: Renderer,
    keys: Keys,
    /// A fixed macOS layout id; `None` follows the current input source.
    keyboard_layout: Option<String>,
    /// The macOS layout the current keymap was generated from.
    active_layout: Option<String>,
    cursor: Option<CursorKey>,
    window: Retained<NSWindow>,
    view: Retained<CompositorView>,
    display_link: Retained<CADisplayLink>,
    /// Display refreshes in a row with nothing drawn or animating.
    idle_frames: u32,
    /// Presentation feedback for the frame drawn at the last refresh, sent
    /// at the next one with that refresh's time.
    pending_feedback: Vec<SurfacePresentationFeedback>,
    /// When the last drawn frame reached the screen: the display link's
    /// target time for it.
    last_target: Duration,
    /// Display refresh counter, the presentation `seq`.
    frame_seq: u64,
    _fd_source: (CFRetained<CFFileDescriptor>, CFRetained<CFRunLoopSource>),
    _key_monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app, unless it isn't set up yet or is already borrowed
/// (an AppKit callback re-entered while we were inside it). Every event
/// comes through here, so it also wakes the display link.
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    with_app_quiet(|app| {
        let result = f(app);
        app.wake();
        result
    })
}

/// `with_app` without waking the display link: for the link's own tick.
fn with_app_quiet<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| match app.try_borrow_mut() {
        Ok(mut app) => app.as_mut().map(f),
        Err(_) => {
            tracing::debug!("re-entrant AppKit callback ignored");
            None
        }
    })
}

/// Display refreshes with nothing to draw before the link pauses.
const IDLE_FRAMES_BEFORE_PAUSE: u32 = 3;

/// Darwin's `CLOCK_UPTIME_RAW` (`<time.h>`): `mach_absolute_time`, which is
/// what `CADisplayLink` timestamps and GLib's `g_get_monotonic_time` count.
pub const CLOCK_UPTIME_RAW: u32 = 8;

impl App {
    /// Something happened: draw at the next refresh if needed.
    fn wake(&mut self) {
        self.idle_frames = 0;
        if self.display_link.isPaused() {
            self.display_link.setPaused(false);
        }
    }

    /// Wayland requests are waiting: dispatch them, then send the replies.
    fn dispatch(&mut self) {
        if let Err(err) = self.event_loop.dispatch(Some(Duration::ZERO), &mut self.state) {
            tracing::error!("event loop: {err}");
        }
        self.flush();
    }

    fn flush(&mut self) {
        if let Err(err) = self.state.display_handle.flush_clients() {
            tracing::warn!("flushing clients: {err}");
        }
    }

    /// Follow the view's size and the window's backing scale.
    fn sync_output(&mut self) {
        let bounds = self.view.bounds();
        let size: Size<i32, Logical> = (bounds.size.width.round() as i32, bounds.size.height.round() as i32).into();
        let scale = self.window.backingScaleFactor();
        if size == self.state.output_size && scale == self.state.output_scale {
            return;
        }
        let refresh = self.window.screen().map(|s| s.maximumFramesPerSecond()).unwrap_or(60).max(1) as i32 * 1000;
        self.renderer.layer.setContentsScale(scale);
        self.renderer.layer.setDrawableSize(CGSize {
            width: size.w as f64 * scale,
            height: size.h as f64 * scale,
        });
        tracing::debug!("output {}x{} points, scale {scale}, {} Hz", size.w, size.h, refresh / 1000);
        self.state.set_output_size(size, scale, refresh);
    }

    /// Generate the keymap from the macOS layout and send it to clients.
    fn apply_keyboard_layout(&mut self) {
        let layout = match &self.keyboard_layout {
            Some(id) => MacLayout::by_id(id),
            None => MacLayout::current(),
        };
        let Some(layout) = layout else {
            tracing::warn!("keyboard layout {:?} not found; keeping the current keymap", self.keyboard_layout);
            return;
        };
        // macOS posts the change notification more than once per switch.
        if self.active_layout.as_deref() == Some(layout.id.as_str()) {
            return;
        }
        let Some(keyboard) = self.state.seat.get_keyboard() else { return };
        match keyboard.set_keymap_from_string(&mut self.state, layout.xkb_keymap()) {
            Ok(()) => {
                tracing::info!("keyboard layout: {} ({})", layout.name, layout.id);
                self.active_layout = Some(layout.id.clone());
            }
            Err(err) => tracing::error!("keymap generated from {} failed to compile: {err:?}", layout.id),
        }
        self.flush();
    }

    /// Show the client's cursor if it changed.
    fn apply_cursor(&mut self) {
        let key = cursor::key(&self.state.cursor_status);
        if self.cursor.as_ref() == Some(&key) {
            return;
        }
        let ns = cursor::ns_cursor(&self.state.cursor_status);
        tracing::debug!(?key, image = ns.is_some(), "cursor");
        self.cursor = Some(key);
        // Applied now if the pointer is over the view; cursor rects keep it
        // when AppKit resets the cursor (entering the window, after a drag).
        let inside = {
            let p = self.window.mouseLocationOutsideOfEventStream();
            let p = self.view.convertPoint_fromView(p, None);
            self.view.mouse_inRect(p, self.view.bounds())
        };
        if inside && self.window.isKeyWindow() {
            ns.clone().unwrap_or_else(NSCursor::arrowCursor).set();
        }
        *self.view.ivars().cursor.borrow_mut() = ns;
        self.window.invalidateCursorRectsForView(&self.view);
    }

    /// One display refresh: draw if anything changed, then let clients draw
    /// their next frame. `now` is this refresh, `target` the next one (when
    /// what's drawn now reaches the screen), both in `CLOCK_UPTIME_RAW`
    /// seconds. Returns false when the window has been closed.
    fn frame(&mut self, now: f64, target: f64) -> bool {
        if !self.window.isVisible() || self.state.quit_requested {
            return false;
        }
        self.sync_output();
        let is_key = self.window.isKeyWindow();
        self.capture.follow(self.mtm, is_key);
        if !is_key && self.keys.any_down() {
            self.keys.release_all(&mut self.state, InputTime::now());
        }
        self.state.children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));

        self.frame_seq += 1;
        // The frame drawn at the last refresh is on screen now.
        let refresh = Refresh::fixed(Duration::from_secs_f64((target - now).max(0.0)));
        for mut feedback in self.pending_feedback.drain(..) {
            feedback.presented(&self.state.output, CLOCK_UPTIME_RAW, self.last_target, refresh, self.frame_seq, wp_presentation_feedback::Kind::Vsync);
        }

        let animating = self.state.animate(std::time::Instant::now());
        if animating {
            self.state.needs_redraw = true;
        }
        let drawn = self.state.needs_redraw;
        if drawn {
            self.state.needs_redraw = false;
            self.renderer.render(&self.state);
            self.pending_feedback = self.state.take_presentation_feedback();
            self.last_target = Duration::from_secs_f64(target.max(0.0));
        }

        self.apply_cursor();
        self.state.ipc_tick();

        let now = self.state.start_time.elapsed();
        let output = self.state.output.clone();
        for window in self.state.space.elements() {
            window.send_frame(&output, now, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        for (layer, _) in self.state.layers_on(&[
            smithay::wayland::shell::wlr_layer::Layer::Background,
            smithay::wayland::shell::wlr_layer::Layer::Bottom,
            smithay::wayland::shell::wlr_layer::Layer::Top,
            smithay::wayland::shell::wlr_layer::Layer::Overlay,
        ]) {
            layer.send_frame(&output, now, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        // Animated cursors wait for frame callbacks too.
        if let smithay::input::pointer::CursorImageStatus::Surface(surface) = &self.state.cursor_status {
            smithay::desktop::utils::send_frames_surface_tree(surface, &output, now, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
        self.state.space.refresh();
        self.state.popups.cleanup();
        smithay::desktop::layer_map_for_output(&output).cleanup();
        self.flush();

        if drawn || animating {
            self.idle_frames = 0;
        } else {
            self.idle_frames += 1;
            if self.idle_frames >= IDLE_FRAMES_BEFORE_PAUSE && self.pending_feedback.is_empty() {
                self.display_link.setPaused(true);
            }
        }
        true
    }
}

#[derive(Default)]
pub struct ViewIvars {
    /// The client's cursor (`None`: the arrow).
    cursor: RefCell<Option<Retained<NSCursor>>>,
}

define_class!(
    /// The window's content: hosts the renderer's `CAMetalLayer` and takes the pointer.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DarwinDesktopCompositorView"]
    #[ivars = ViewIvars]
    pub struct CompositorView;

    impl CompositorView {
        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            if let Some(cursor) = &*self.ivars().cursor.borrow() {
                self.addCursorRect_cursor(self.bounds(), cursor);
            }
        }

        /// Wayland's y axis points down.
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        /// The click that activates the window also reaches the client.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, event: &NSEvent) {
            self.motion(event);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            self.motion(event);
        }

        #[unsafe(method(rightMouseDragged:))]
        fn right_mouse_dragged(&self, event: &NSEvent) {
            self.motion(event);
        }

        #[unsafe(method(otherMouseDragged:))]
        fn other_mouse_dragged(&self, event: &NSEvent) {
            self.motion(event);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.button(event, BTN_LEFT, true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, event: &NSEvent) {
            self.button(event, BTN_LEFT, false);
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, event: &NSEvent) {
            self.button(event, BTN_RIGHT, true);
        }

        #[unsafe(method(rightMouseUp:))]
        fn right_mouse_up(&self, event: &NSEvent) {
            self.button(event, BTN_RIGHT, false);
        }

        #[unsafe(method(otherMouseDown:))]
        fn other_mouse_down(&self, event: &NSEvent) {
            self.button(event, other_button(event), true);
        }

        #[unsafe(method(otherMouseUp:))]
        fn other_mouse_up(&self, event: &NSEvent) {
            self.button(event, other_button(event), false);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, event: &NSEvent) {
            with_app(|app| {
                input::scroll(&mut app.state, event);
                app.flush();
            });
        }

        // Trackpad touches (indirect touches), for gestures.
        #[unsafe(method(touchesBeganWithEvent:))]
        fn touches_began(&self, event: &NSEvent) {
            with_app(|app| crate::gesture::touches(&mut app.state, event));
        }

        #[unsafe(method(touchesMovedWithEvent:))]
        fn touches_moved(&self, event: &NSEvent) {
            with_app(|app| crate::gesture::touches(&mut app.state, event));
        }

        #[unsafe(method(touchesEndedWithEvent:))]
        fn touches_ended(&self, event: &NSEvent) {
            with_app(|app| crate::gesture::touches(&mut app.state, event));
        }

        #[unsafe(method(touchesCancelledWithEvent:))]
        fn touches_cancelled(&self, event: &NSEvent) {
            with_app(|app| crate::gesture::touches(&mut app.state, event));
        }

        /// The display link: one call per display refresh.
        #[unsafe(method(tick:))]
        fn tick(&self, link: &CADisplayLink) {
            let (now, target) = (link.timestamp(), link.targetTimestamp());
            if with_app_quiet(|app| app.frame(now, target)) == Some(false) {
                quit(self.mtm());
            }
        }
    }
);

impl CompositorView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars::default());
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        // Raw trackpad touches, for gestures (gesture.rs).
        view.setAllowedTouchTypes(objc2_app_kit::NSTouchTypeMask::Indirect);
        view
    }

    fn motion(&self, event: &NSEvent) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        with_app(|app| {
            input::pointer_motion(&mut app.state, (p.x, p.y).into(), event_time(event));
            app.flush();
        });
    }

    fn button(&self, event: &NSEvent, button: u32, pressed: bool) {
        let p = self.convertPoint_fromView(event.locationInWindow(), None);
        with_app(|app| {
            let time = event_time(event);
            input::pointer_motion(&mut app.state, (p.x, p.y).into(), time);
            input::pointer_button(&mut app.state, button, pressed, time);
            app.flush();
        });
    }
}

/// `buttonNumber` 2 is the middle button, 3 and 4 back and forward.
fn other_button(event: &NSEvent) -> u32 {
    match event.buttonNumber() {
        2 => BTN_MIDDLE,
        n => 0x110 + n as u32, // BTN_SIDE, BTN_EXTRA, ...
    }
}

fn quit(mtm: MainThreadMarker) {
    // The window server would restore the hot key mode at exit anyway; be tidy.
    with_app(|app| app.capture.stop(mtm));
    NSApplication::sharedApplication(mtm).terminate(None);
}

/// macOS switched keyboard input source (menu bar, Settings, another app).
unsafe extern "C" fn input_source_changed(
    _center: *mut c_void,
    _observer: *mut c_void,
    _name: *const c_void,
    _object: *const c_void,
    _info: *const c_void,
) {
    with_app(|app| {
        if app.keyboard_layout.is_none() {
            app.apply_keyboard_layout();
        }
    });
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFNotificationCenterGetDistributedCenter() -> *mut c_void;
    fn CFNotificationCenterAddObserver(
        center: *mut c_void,
        observer: *const c_void,
        callback: unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_void, *const c_void, *const c_void),
        name: *const c_void,
        object: *const c_void,
        suspension_behavior: isize,
    );
}
/// CFNotificationSuspensionBehaviorDeliverImmediately
const DELIVER_IMMEDIATELY: isize = 4;

/// Wakes the display link on window changes that come with no event of
/// ours: losing or gaining key status (keyboard capture, held keys),
/// resizing, the backing scale or screen changing. The observers live as
/// long as the process.
fn observe_window(window: &NSWindow) {
    use objc2_app_kit::{
        NSWindowDidBecomeKeyNotification, NSWindowDidChangeBackingPropertiesNotification,
        NSWindowDidChangeOcclusionStateNotification, NSWindowDidChangeScreenNotification, NSWindowDidResignKeyNotification,
        NSWindowDidResizeNotification,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter};
    let center = NSNotificationCenter::defaultCenter();
    // SAFETY: the names are AppKit's constant strings.
    let names = unsafe {
        [
            NSWindowDidBecomeKeyNotification,
            NSWindowDidResignKeyNotification,
            NSWindowDidResizeNotification,
            NSWindowDidChangeBackingPropertiesNotification,
            NSWindowDidChangeScreenNotification,
            NSWindowDidChangeOcclusionStateNotification,
        ]
    };
    for name in names {
        let block = RcBlock::new(|_: NonNull<NSNotification>| {
            with_app(|_| ());
        });
        // SAFETY: the block is called on the main queue's thread (queue None
        // means the posting thread, and AppKit posts these on the main thread).
        let observer = unsafe { center.addObserverForName_object_queue_usingBlock(Some(name), Some(window), None, &block) };
        std::mem::forget(observer);
    }
}

/// Hands the session bus the variables its activated services need to reach
/// us, as Hyprland does at startup: the bus starts before the compositor
/// (`dbus-run-session` in the session), so a service it starts on demand
/// (dunst on the first notification) would otherwise find no
/// `WAYLAND_DISPLAY`. Skipped without a session bus.
fn update_activation_environment(state: &mut State, socket: &str, runtime_dir: &str) {
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        return;
    }
    let mut vars = vec![
        format!("WAYLAND_DISPLAY={socket}"),
        format!("XDG_RUNTIME_DIR={runtime_dir}"),
        "XDG_SESSION_TYPE=wayland".to_string(),
    ];
    if let Ok(signature) = std::env::var("HYPRLAND_INSTANCE_SIGNATURE") {
        vars.push(format!("HYPRLAND_INSTANCE_SIGNATURE={signature}"));
    }
    vars.extend(state.config.env().into_iter().map(|(k, v)| format!("{k}={v}")));
    match Command::new("dbus-update-activation-environment").args(&vars).spawn() {
        Ok(child) => state.children.push(child),
        Err(err) => tracing::warn!("dbus-update-activation-environment: {err}"),
    }
}

/// The calloop fd became readable.
unsafe extern "C-unwind" fn loop_fd_ready(fd: *mut CFFileDescriptor, _types: CFOptionFlags, _info: *mut c_void) {
    with_app(App::dispatch);
    // CFFileDescriptor callbacks are one-shot.
    if let Some(fd) = unsafe { fd.as_ref() } {
        fd.enable_call_backs(kCFFileDescriptorReadCallBack);
    }
}

pub fn run(mtm: MainThreadMarker, options: Options) {
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);

    // Wayland side.
    let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("event loop");
    let display: Display<State> = Display::new().expect("Wayland display");
    let config = crate::config::Config::load(options.config.as_deref());
    let state = State::new(display, &event_loop.handle(), config);
    let socket = state.socket_name.to_string_lossy().into_owned();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    tracing::info!("listening on {socket} (XDG_RUNTIME_DIR={runtime_dir})");
    println!("WAYLAND_DISPLAY={socket} XDG_RUNTIME_DIR={runtime_dir}");
    let mut state = state;
    update_activation_environment(&mut state, &socket, &runtime_dir);

    // The window: 80% of the main screen, or full screen with --fullscreen.
    let screen = NSScreen::mainScreen(mtm)
        .map(|s| s.visibleFrame())
        .unwrap_or(NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1440.0, 900.0)));
    let size = NSSize::new((screen.size.width * 0.8).round(), (screen.size.height * 0.8).round());
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), size);
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            frame,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(&format!("compositor ({socket})")));
    window.setAcceptsMouseMovedEvents(true);

    let renderer = Renderer::new();
    let view = CompositorView::new(mtm, frame);
    // Layer-hosting: our layer, drawn only by us.
    view.setLayer(Some(&renderer.layer));
    view.setWantsLayer(true);
    window.setContentView(Some(&view));
    window.makeFirstResponder(Some(&view));
    window.center();

    // calloop's fd on the main run loop.
    let fd_source = {
        use std::os::fd::AsRawFd;
        let fd = event_loop.as_raw_fd();
        let cf_fd = unsafe { CFFileDescriptor::new(None, fd, false, Some(loop_fd_ready), ptr::null()) }
            .expect("CFFileDescriptor");
        cf_fd.enable_call_backs(kCFFileDescriptorReadCallBack);
        let source = CFFileDescriptor::new_run_loop_source(None, Some(&cf_fd), 0).expect("run loop source");
        CFRunLoop::current().expect("run loop").add_source(Some(&source), unsafe { kCFRunLoopCommonModes });
        (cf_fd, source)
    };

    // Frames at the display's refresh rate.
    let display_link = unsafe { view.displayLinkWithTarget_selector(&view, sel!(tick:)) };
    unsafe { display_link.addToRunLoop_forMode(&NSRunLoop::currentRunLoop(), NSRunLoopCommonModes) };

    // Keys for our window, before menus and AppKit's key handling.
    let window_ptr: *const NSWindow = &*window;
    let monitor_block = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
        // SAFETY: AppKit passes a valid event for the duration of the call.
        let ev = unsafe { event.as_ref() };
        let ours = ev.window(mtm).is_some_and(|w| ptr::eq(&*w, window_ptr));
        if !ours {
            return event.as_ptr();
        }
        match ev.r#type() {
            NSEventType::KeyDown if input::is_quit_chord(ev) => {
                tracing::info!("Cmd+Shift+Q: leaving");
                quit(mtm);
            }
            NSEventType::KeyDown | NSEventType::KeyUp => {
                let pressed = ev.r#type() == NSEventType::KeyDown;
                with_app(|app| {
                    app.keys.key_event(&mut app.state, ev, pressed);
                    app.flush();
                });
            }
            NSEventType::FlagsChanged => {
                with_app(|app| {
                    app.keys.modifier_event(&mut app.state, ev);
                    app.flush();
                });
            }
            _ => return event.as_ptr(),
        }
        ptr::null_mut()
    });
    let key_monitor = unsafe {
        NSEvent::addLocalMonitorForEventsMatchingMask_handler(
            NSEventMask::KeyDown | NSEventMask::KeyUp | NSEventMask::FlagsChanged,
            &monitor_block,
        )
    };

    APP.with(|cell| {
        *cell.borrow_mut() = Some(App {
            mtm,
            capture: Capture::new(options.capture),
            event_loop,
            state,
            renderer,
            keys: Keys::default(),
            keyboard_layout: options.keyboard_layout.clone(),
            active_layout: None,
            cursor: None,
            window: window.clone(),
            view: view.clone(),
            display_link,
            idle_frames: 0,
            pending_feedback: Vec::new(),
            last_target: Duration::ZERO,
            frame_seq: 0,
            _fd_source: fd_source,
            _key_monitor: key_monitor,
        });
    });
    with_app(App::sync_output);
    with_app(App::apply_keyboard_layout);
    observe_window(&window);
    // SAFETY: the callback is a plain function; the name is a static CFString.
    unsafe {
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDistributedCenter(),
            ptr::null(),
            input_source_changed,
            layout::kTISNotifySelectedKeyboardInputSourceChanged as *const _ as *const c_void,
            ptr::null(),
            DELIVER_IMMEDIATELY,
        );
    }

    // The config's top-level hl.exec_cmd calls and hl.on("hyprland.start") hooks.
    with_app(|app| {
        let mut actions = app.state.config.take_queue();
        actions.extend(app.state.config.run_start_hooks());
        for action in actions {
            app.state.run_action(action);
        }
    });

    if !options.command.is_empty() {
        let mut command = Command::new(&options.command[0]);
        command.args(&options.command[1..]).env("WAYLAND_DISPLAY", &socket).env("XDG_RUNTIME_DIR", &runtime_dir);
        match command.spawn() {
            Ok(child) => {
                tracing::info!("started {:?} (pid {})", options.command[0], child.id());
                with_app(|app| app.state.children.push(child));
            }
            Err(err) => tracing::error!("starting {:?}: {err}", options.command[0]),
        }
    }

    window.makeKeyAndOrderFront(None);
    app.activate();
    if options.fullscreen {
        window.toggleFullScreen(None);
    }
    app.run();
}
