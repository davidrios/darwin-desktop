//! The AppKit side: the window, its Metal-backed view, the run loop.
//!
//! Event loop (outline section 6, item 4): AppKit owns the main thread. The
//! compositor's calloop loop (Wayland sockets) is driven from the same run
//! loop: its kqueue fd is wrapped in a `CFFileDescriptor`, and when it turns
//! readable the loop is dispatched without blocking. Frames come from the
//! view's `CADisplayLink` at the display's refresh rate. Everything runs on
//! the main thread, so the state lives in a thread-local.
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
    MainThreadMarker, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
    sel,
};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSBackingStoreType, NSEvent, NSEventMask, NSEventType, NSResponder,
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
    reexports::{calloop::EventLoop, wayland_server::Display},
    utils::{Logical, Size},
};

use crate::{
    Options,
    input::{self, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, Keys, event_time},
    render::Renderer,
    state::State,
};

pub struct App {
    event_loop: EventLoop<'static, State>,
    state: State,
    renderer: Renderer,
    keys: Keys,
    window: Retained<NSWindow>,
    view: Retained<CompositorView>,
    /// Kept alive for the life of the app.
    _display_link: Retained<CADisplayLink>,
    _fd_source: (CFRetained<CFFileDescriptor>, CFRetained<CFRunLoopSource>),
    _key_monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static APP: RefCell<Option<App>> = const { RefCell::new(None) };
}

/// Runs `f` on the app, unless it isn't set up yet or is already borrowed
/// (an AppKit callback re-entered while we were inside it).
fn with_app<R>(f: impl FnOnce(&mut App) -> R) -> Option<R> {
    APP.with(|app| match app.try_borrow_mut() {
        Ok(mut app) => app.as_mut().map(f),
        Err(_) => {
            tracing::debug!("re-entrant AppKit callback ignored");
            None
        }
    })
}

impl App {
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

    /// One display refresh: draw if anything changed, then let clients draw
    /// their next frame. Returns false when the window has been closed.
    fn frame(&mut self) -> bool {
        if !self.window.isVisible() {
            return false;
        }
        self.sync_output();
        if !self.window.isKeyWindow() && self.keys.any_down() {
            self.keys.release_all(&mut self.state, InputTime::now());
        }
        self.state.children.retain_mut(|child| matches!(child.try_wait(), Ok(None)));

        if self.state.needs_redraw {
            self.state.needs_redraw = false;
            self.renderer.render(&self.state);
        }

        let now = self.state.start_time.elapsed();
        let output = self.state.output.clone();
        for window in self.state.space.elements() {
            window.send_frame(&output, now, Some(Duration::ZERO), |_, _| Some(output.clone()));
        }
        self.state.space.refresh();
        self.state.popups.cleanup();
        self.flush();
        true
    }
}

define_class!(
    /// The window's content: hosts the renderer's `CAMetalLayer` and takes the pointer.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "DarwinDesktopCompositorView"]
    pub struct CompositorView;

    impl CompositorView {
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

        /// The display link: one call per display refresh.
        #[unsafe(method(tick:))]
        fn tick(&self, _link: &CADisplayLink) {
            if with_app(App::frame) == Some(false) {
                quit(self.mtm());
            }
        }
    }
);

impl CompositorView {
    fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        unsafe { msg_send![super(this), initWithFrame: frame] }
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
    NSApplication::sharedApplication(mtm).terminate(None);
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
    let state = State::new(display, &event_loop.handle());
    let socket = state.socket_name.to_string_lossy().into_owned();
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    tracing::info!("listening on {socket} (XDG_RUNTIME_DIR={runtime_dir})");
    println!("WAYLAND_DISPLAY={socket} XDG_RUNTIME_DIR={runtime_dir}");

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
            event_loop,
            state,
            renderer,
            keys: Keys::default(),
            window: window.clone(),
            view: view.clone(),
            _display_link: display_link,
            _fd_source: fd_source,
            _key_monitor: key_monitor,
        });
    });
    with_app(App::sync_output);

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
