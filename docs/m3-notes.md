# M3 notes: a session you can live in

Plan and order of steps: `m3-plan.md`. Scope decisions: `decisions.md` (2026-11-24).

## Step 1: frame pacing, damage and power (2026-11-24, not yet checked by hand)

| Piece | File | How |
|---|---|---|
| Idle display link | `app.rs` | The `CADisplayLink` pauses after 3 refreshes in a row with nothing drawn, no animation running and no presentation feedback owed. Every event resumes it: everything reaches the app through `with_app`, which wakes the link (the tick itself uses `with_app_quiet`). Window notifications with no event of ours (key status, resize, backing scale, screen, occlusion) are observed so they wake it too; keyboard capture and releasing held keys still follow key status. |
| Presentation time | `app.rs`, `state.rs` | `wp_presentation` (Smithay) on Darwin's `CLOCK_UPTIME_RAW` (8): `mach_absolute_time`, the clock of `CADisplayLink` timestamps and of GLib's `g_get_monotonic_time` on macOS, so GTK's frame clock and our times agree. After a frame is drawn, the feedback of every surface drawn (windows and popups, layer surfaces, the cursor) is taken. At the next refresh it's sent as presented, with the link's target time for that frame, the refresh interval and a refresh counter as `seq` (kind `vsync`). |
| Damage uploads | `render.rs` | A surface's cached texture holds its contents as of its last uploaded commit. When the buffer size is unchanged, only the buffer damage since that commit is uploaded (`RendererSurfaceState::damage_since`), whichever of the client's buffers is attached now. Above 16 rectangles, or on a new size, the buffer is uploaded whole. |

To check by hand:
- Idle CPU in Activity Monitor with VTE and waybar open and nothing moving: should be near zero, where before it ticked at 60 Hz.
- Typing in VTE, scrolling in GTK apps, animations, workspace swipes, live resize: all still smooth, nothing stale on screen.
- Whether GTK's `gdk_frame_timings_*` warnings are gone.
- Focus: Cmd+Tab away with keys held and come back (capture and held-key release must still follow when the session is idle).

Not done yet:
- **Output damage:** the whole frame is still redrawn whenever anything changed, blur passes included. Caching the background blur until the layers under it change is the next saving if idle-but-typing costs show up.
- **Covered windows:** windows under a fullscreen window still get frame callbacks (hidden workspaces don't: their windows aren't mapped).
- **120 Hz:** `CADisplayLink` from an `NSView` follows the screen's rate; there's no ProMotion panel here to check it on.
- **Texture reuse while in flight:** a texture being rewritten may still be read by the previous frame's command buffer (true of whole uploads before this change too). Not seen; double-buffered textures would fix it if it shows.

## Step 2: clipboard (2026-11-25, not yet checked by hand)

No bridge to the macOS pasteboard (owner, `decisions.md` 2026-11-25). Copy and paste between clients already worked through `wl_data_device`.

| Piece | File | How |
|---|---|---|
| Primary selection | `state.rs` | `zwp_primary_selection_device_manager_v1` (Smithay). Its focus follows keyboard focus alongside the data device's, so the focused client can read and set it. GTK and VTE use it as soon as it's advertised: select text, then middle-click to paste. |
| Data-control | `state.rs` | `ext_data_control_manager_v1` and `zwlr_data_control_manager_v1` (Smithay), both covering the primary selection too. Any client may use them, as in Hyprland: clipboard tools read and set the selection without focus. |
| wl-clipboard | `nix/overlay.nix`, `nix/pkgs/session.nix` | wl-clipboard 2.3.0 builds unpatched against the scope's `wayland`. `wl-copy`/`wl-paste` are on the session's `PATH` and exported by the flake. |

To check by hand:
- Select text in VTE and middle-click in a GTK entry, and the other way round.
- In VTE: `echo hi | wl-copy`, then Ctrl+V in a GTK app; copy in a GTK app, then `wl-paste`; `wl-paste --primary` after a selection; `wl-paste --list-types`.

Not done yet:
- How bemtvi's TUI reaches the clipboard inside VTE (OSC 52 or `wl-copy`): with bemtvi, step 5.
- No limit on which clients may use data-control (a client can read the clipboard without focus). Same as Hyprland's default; a security-context filter could come later.

## Step 3: session bus, audio and notifications (2026-11-25, not yet checked by hand)

| Piece | File | How |
|---|---|---|
| Session bus | `nix/pkgs/session.nix` | The session runs the compositor under `dbus-run-session`, so the bus lives exactly as long as the session and `DBUS_SESSION_BUS_ADDRESS` reaches every client. Nixpkgs' dbus looks for its config under `/etc`, so the session passes the `session.conf` it ships. That config listens on `unix:tmpdir=/tmp`: a short path, under macOS's 103-byte socket limit. Services activate from `XDG_DATA_DIRS/dbus-1/services`. |
| `XDG_RUNTIME_DIR` | `nix/pkgs/session.nix` | Set by the session (`$TMPDIR/darwin-desktop-runtime` unless already set), so the compositor, PulseAudio and clients agree on it. The compositor still creates the same directory when run on its own. |
| Sound server | `nix/pkgs/session.nix`, `nix/overlay.nix` | PulseAudio 17 (nixpkgs builds it for Darwin with CoreAudio modules) started by the session script and stopped on exit, unless one already answers. Its script loads `module-coreaudio-detect record=no` (every output device, with hotplug), the restore modules, `always-sink` and `suspend-on-idle`. Anonymous auth on the unix socket, which sits in the private runtime directory. `PULSE_SERVER` is exported. |
| Microphone patch | `patches/pulseaudio/001` | With `record=no`, the detect module still loaded the device module for the built-in microphone, which creates an IOProc on it. CoreAudio then checks the microphone permission: startup blocked in `AudioDeviceCreateIOProcID` until the prompt was answered, or for about a minute when no prompt could be shown (seen from a shell). The main loop wasn't running, so the daemon ignored clients and SIGTERM. The patch skips devices with no streams in an enabled direction. |
| Notifications | `nix/overlay.nix`, `nix/pkgs/session.nix`, `patches/dunst/001` | dunst 1.13.2 (owner's choice over mako, `decisions.md`), Wayland only, X11 and systemd off. The patch: Darwin's `st_mtimespec`, a global `wait` in dunstify that collides with libc's `wait()`, no `-lrt`; `linux/input-event-codes.h` comes from the scope's `linux-input-headers`. Its D-Bus service file is on `XDG_DATA_DIRS`, so the first notification starts it. Without a `dunstrc` it uses built-in defaults. |
| Activation environment | `compositor/src/app.rs` | The bus starts before the compositor, so services it activates had no `WAYLAND_DISPLAY` (seen headless: dunst exits with "No applicable output"). At startup, with a session bus present, the compositor runs `dbus-update-activation-environment` with `WAYLAND_DISPLAY`, `XDG_RUNTIME_DIR`, `XDG_SESSION_TYPE`, `HYPRLAND_INSTANCE_SIGNATURE` and the config's `hl.env` variables, as Hyprland does. |
| Tools | `nix/pkgs/session.nix`, `nix/overlay.nix` | `pactl`, `pamixer` (nixpkgs marks it Linux-only; it builds unchanged), `notify-send` (libnotify), `playerctl` on the session's `PATH`. |

Checked headless (2026-11-25), with the session script running a shell in place of the compositor: the bus answers; PulseAudio answers about 5 s after start with the MacBook speakers as default sink (CoreAudio name `1__2`); `pamixer --get-volume-human` and `playerctl -l` work; nothing is left running after exit.

To check by hand:
- In the session's terminal: `pactl info`; `paplay /System/Library/Sounds/Glass.aiff` (libsndfile reads WAV, AIFF, FLAC and Ogg, not MP3; checked headless into a null sink); `pamixer -i 3` / `-d 3` changes the volume.
- `notify-send hello`: a dunst popup at the top right; `dunstctl history` lists it.

Not done yet:
- **mako's `hl.exec_cmd("mako")`** in the owner's config finds no mako (harmless, logged); dunst needs no exec line.
- **Microphone:** closed (`record=no`). Turning it on means the session's app bundle (step 7) asks for permission once.
- **Device names:** CoreAudio sinks get numeric names (`1__2`); descriptions are readable.
- **GStreamer** has only gst-plugins-base on its path: no `pulsesink` yet, so GStreamer apps don't play through PulseAudio.
- **Noise:** PulseAudio logs a `caps.c` warning at startup (built without capabilities). Harmless.

Also fixed: `config::tests::load_str` wrote both tests that load the example config to the same temporary file, so they raced when run in parallel (an intermittent failure of `group_helpers_see_the_group`).
