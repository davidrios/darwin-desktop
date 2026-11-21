//! Hyprland-style Wayland compositor for macOS (M1 skeleton).
//!
//! Runs as an ordinary AppKit app (Stage A in docs/PROJECT_OUTLINE.md): one
//! window drawn with Metal, Wayland clients composited into it, keyboard and
//! pointer forwarded from AppKit. Smithay handles the protocol state.
//!
//! Usage: compositor [--fullscreen] [--no-capture] [-- client-command args...]

mod app;
mod capture;
mod cursor;
mod input;
mod keymap;
mod render;
mod state;

use std::ffi::OsString;

use objc2::MainThreadMarker;
use tracing_subscriber::EnvFilter;

pub struct Options {
    pub fullscreen: bool,
    /// Take the system's shortcuts (Cmd+Tab, Cmd+Space, ...) while our window is key.
    pub capture: bool,
    /// A client to start once the socket is up, with WAYLAND_DISPLAY set.
    pub command: Vec<OsString>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let mut options = Options { fullscreen: false, capture: true, command: Vec::new() };
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") => {
                options.command = args.collect();
                break;
            }
            Some("--fullscreen") => options.fullscreen = true,
            Some("--no-capture") => options.capture = false,
            Some("-h" | "--help") => {
                println!("usage: compositor [--fullscreen] [--no-capture] [-- client-command args...]");
                println!("Cmd+Shift+Q quits. While the window is focused, system shortcuts");
                println!("(Cmd+Tab, Cmd+Space, Ctrl+arrows...) go to clients unless --no-capture.");
                return;
            }
            _ => {
                eprintln!("unknown argument {arg:?} (try --help)");
                std::process::exit(2);
            }
        }
    }

    ensure_runtime_dir();

    let mtm = MainThreadMarker::new().expect("the compositor must start on the main thread");
    app::run(mtm, options);
}

/// libwayland and Smithay put the socket in XDG_RUNTIME_DIR, which macOS
/// doesn't set. Use a private directory under TMPDIR (per user, per boot).
fn ensure_runtime_dir() {
    if std::env::var_os("XDG_RUNTIME_DIR").is_some() {
        return;
    }
    let base = std::env::temp_dir().join("darwin-desktop-runtime");
    if let Err(err) = std::fs::create_dir_all(&base) {
        panic!("creating {}: {err}", base.display());
    }
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
    // SAFETY: single-threaded at this point, before AppKit or any thread starts.
    unsafe { std::env::set_var("XDG_RUNTIME_DIR", &base) };
}
