//! Hyprland-style Wayland compositor for macOS (M1 skeleton).
//!
//! Runs as an ordinary AppKit app (Stage A in docs/PROJECT_OUTLINE.md): one
//! window drawn with Metal, Wayland clients composited into it, keyboard and
//! pointer forwarded from AppKit. Smithay handles the protocol state.
//!
//! Usage: compositor [--fullscreen] [--no-capture] [--keyboard-layout ID] [--dump-keymap] [-- client args...]

mod anim;
mod app;
mod binds;
mod config;
mod gesture;
mod dwindle;
mod capture;
mod cursor;
mod input;
mod ipc;
mod keymap;
mod layers;
mod layout;
mod mouse;
mod render;
mod rules;
mod state;
mod wm;

use std::ffi::OsString;

use objc2::MainThreadMarker;
use tracing_subscriber::EnvFilter;

pub struct Options {
    pub fullscreen: bool,
    /// Take the system's shortcuts (Cmd+Tab, Cmd+Space, ...) while our window is key.
    pub capture: bool,
    /// A macOS layout id to use instead of following the current input source.
    pub keyboard_layout: Option<String>,
    /// The Lua config (default ~/.config/hypr/hyprland.lua).
    pub config: Option<std::path::PathBuf>,
    /// A client to start once the socket is up, with WAYLAND_DISPLAY set.
    pub command: Vec<OsString>,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let mut options = Options { fullscreen: false, capture: true, keyboard_layout: None, config: None, command: Vec::new() };
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--") => {
                options.command = args.collect();
                break;
            }
            Some("--fullscreen") => options.fullscreen = true,
            Some("--no-capture") => options.capture = false,
            Some("--config") => {
                let Some(path) = args.next() else {
                    eprintln!("--config needs a path to a hyprland.lua");
                    std::process::exit(2);
                };
                options.config = Some(path.into());
            }
            Some("--keyboard-layout") => {
                let Some(id) = args.next().and_then(|a| a.into_string().ok()) else {
                    eprintln!("--keyboard-layout needs a macOS layout id, e.g. com.apple.keylayout.Brazilian-Pro");
                    std::process::exit(2);
                };
                options.keyboard_layout = Some(id);
            }
            Some("--dump-keymap") => {
                let layout = match &options.keyboard_layout {
                    Some(id) => layout::MacLayout::by_id(id),
                    None => layout::MacLayout::current(),
                };
                match layout {
                    Some(layout) => print!("{}", layout.xkb_keymap()),
                    None => {
                        eprintln!("no such keyboard layout");
                        std::process::exit(1);
                    }
                }
                return;
            }
            Some("-h" | "--help") => {
                println!("usage: compositor [--config hyprland.lua] [--fullscreen] [--no-capture]");
                println!("                  [--keyboard-layout ID] [--dump-keymap]");
                println!("                  [-- client-command args...]");
                println!("Config: Hyprland's Lua config, default ~/.config/hypr/hyprland.lua (SUPER is Cmd).");
                println!("The keymap is generated from the macOS keyboard layout: the current input");
                println!("source (followed as it changes), or ID, e.g. com.apple.keylayout.Brazilian-Pro.");
                println!("--dump-keymap prints the generated xkb keymap (put --keyboard-layout first).");
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
