//! Keyboard capture (docs/decisions.md, "keyboard capture in Stage A follows 2ksbox").
//!
//! While our window is the key window, the window server's hot key mode is set
//! to "all disabled except Universal Access" with SkyLight's private
//! `CGSSetGlobalHotKeyOperatingMode`: Cmd+Tab, Cmd+Space, Mission Control and
//! the Spaces arrows (Ctrl+arrows), the screenshot chords and the rest of the
//! Keyboard settings pane's shortcuts then arrive as ordinary key events. The
//! approach is the one UTM, VirtualBox and 2ksbox/mitsuami use.
//!
//! The symbols are looked up at run time, so a macOS without them still
//! launches; then the presentation options cover Cmd+Tab and Cmd+H only.
//!
//! Measured 2026-11-21 (macOS 26.7): the mode belongs to the process that set
//! it; the window server restores it when that process exits, even after
//! `kill -9`. A crash can't leave the Mac without its shortcuts.

use std::ffi::{c_char, c_void};

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationPresentationOptions};

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

type ConnFn = unsafe extern "C" fn() -> i32;
type GetFn = unsafe extern "C" fn(conn: i32, mode: *mut i32) -> i32;
type SetFn = unsafe extern "C" fn(conn: i32, mode: i32) -> i32;

/// Every Keyboard-pane shortcut off, the accessibility ones kept.
const ALL_DISABLED_BUT_UNIVERSAL_ACCESS: i32 = 2;

struct HotKeyMode {
    conn: i32,
    get: GetFn,
    set: SetFn,
}

impl HotKeyMode {
    fn lookup() -> Option<HotKeyMode> {
        // SAFETY: when present, the symbols have these signatures.
        unsafe {
            let conn = dlsym(RTLD_DEFAULT, c"CGSMainConnectionID".as_ptr());
            let get = dlsym(RTLD_DEFAULT, c"CGSGetGlobalHotKeyOperatingMode".as_ptr());
            let set = dlsym(RTLD_DEFAULT, c"CGSSetGlobalHotKeyOperatingMode".as_ptr());
            if conn.is_null() || get.is_null() || set.is_null() {
                return None;
            }
            let conn = std::mem::transmute::<*mut c_void, ConnFn>(conn)();
            Some(HotKeyMode {
                conn,
                get: std::mem::transmute::<*mut c_void, GetFn>(get),
                set: std::mem::transmute::<*mut c_void, SetFn>(set),
            })
        }
    }

    fn get(&self) -> Option<i32> {
        let mut mode = -1;
        // SAFETY: a plain call on our own connection.
        (unsafe { (self.get)(self.conn, &mut mode) } == 0).then_some(mode)
    }

    fn set(&self, mode: i32) -> bool {
        // SAFETY: as above.
        unsafe { (self.set)(self.conn, mode) == 0 }
    }
}

/// What capture turned off, to give back.
enum Held {
    HotKeys(i32),
    Presentation(NSApplicationPresentationOptions),
}

pub struct Capture {
    enabled: bool,
    hot_keys: Option<HotKeyMode>,
    held: Option<Held>,
}

impl Capture {
    pub fn new(enabled: bool) -> Capture {
        let hot_keys = HotKeyMode::lookup();
        if enabled && hot_keys.is_none() {
            tracing::warn!("hot key mode unavailable: capturing Cmd+Tab and Cmd+H only (presentation options)");
        }
        Capture { enabled, hot_keys, held: None }
    }

    /// Follow the window's key status: capture while key, release otherwise.
    pub fn follow(&mut self, mtm: MainThreadMarker, window_is_key: bool) {
        if window_is_key && self.enabled && self.held.is_none() {
            self.start(mtm);
        } else if !window_is_key && self.held.is_some() {
            self.stop(mtm);
        }
    }

    fn start(&mut self, mtm: MainThreadMarker) {
        if let Some(hk) = &self.hot_keys {
            if let Some(before) = hk.get() {
                if hk.set(ALL_DISABLED_BUT_UNIVERSAL_ACCESS) && hk.get() == Some(ALL_DISABLED_BUT_UNIVERSAL_ACCESS) {
                    tracing::info!("keyboard captured (system shortcuts off)");
                    self.held = Some(Held::HotKeys(before));
                    return;
                }
                hk.set(before);
            }
            tracing::warn!("the window server refused the hot key mode; falling back to presentation options");
        }
        let app = NSApplication::sharedApplication(mtm);
        let before = app.presentationOptions();
        // Disabling process switching requires the Dock to be hidden or auto-hidden.
        let options = before
            | NSApplicationPresentationOptions::AutoHideDock
            | NSApplicationPresentationOptions::DisableProcessSwitching
            | NSApplicationPresentationOptions::DisableHideApplication;
        app.setPresentationOptions(options);
        tracing::info!("keyboard captured (presentation options: Cmd+Tab, Cmd+H)");
        self.held = Some(Held::Presentation(before));
    }

    pub fn stop(&mut self, mtm: MainThreadMarker) {
        match self.held.take() {
            Some(Held::HotKeys(before)) => {
                if let Some(hk) = &self.hot_keys {
                    hk.set(before);
                }
                tracing::info!("keyboard released (system shortcuts back)");
            }
            Some(Held::Presentation(before)) => {
                NSApplication::sharedApplication(mtm).setPresentationOptions(before);
                tracing::info!("keyboard released");
            }
            None => {}
        }
    }
}
