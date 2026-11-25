# M3 plan: a session you can live in (2026-11-24)

Outline section 4, M3. **Exit:** a full working day inside the session without touching the Mac desktop.

Reviewed with the project owner on 2026-11-24 (answers below). Steps are ordered by dependency and risk, each one checked by hand before the next, as in M2.

## Scope decisions (owner, 2026-11-24)

- **Browsers: Firefox daily, Chrome for some things. A Mac browser beside the session is not acceptable**, so M3 can't exit without a browser in the session.
- **Editor: bemtvi** (the owner's editor, `~/work/bemtvi`), mostly in terminal mode. Its GUI is built on mitsuami (`~/work/rust-ui-toolkit`); making it work is the ideal.
- **No Qt app in daily use:** Qt 6 moves out of M3.
- **2ksbox** (`~/work/win-98-xp-virt`) should run in the session too; it's a mitsuami app like bemtvi's GUI.
- **One monitor** (the built-in panel) for now: multi-monitor moves out of M3.
- **No input methods** for now.

## What "a working day" needs, measured against the owner's config

`~/.config/hypr/hyprland.lua` (copy in `docs/exampleconfig.lua`) names the tools a day is built from:

| Need | In the config | State today | M3 answer |
|---|---|---|---|
| Terminal | wezterm | VTE sample app | Keep VTE (wezterm needs EGL or Vulkan on Wayland: M4). |
| Editor | bemtvi | not tried | TUI in VTE (step 5); GUI through mitsuami's GTK backend (step 5, may finish in M4). |
| Browser | Firefox, Chrome | none | Firefox: step 0 and step 6. Chrome: after M3. |
| File manager | nautilus | none | Deferred (needs GLib desktop app info, see step 6). |
| Launcher | fzf in a terminal (`fzf-launcher`, `-desktop`) | scripts not ported | Works once the scripts find `.desktop` files; no GIO needed. |
| Bar | waybar | runs | Check its Hyprland modules in detail. |
| Notifications | mako | none | dunst (owner, 2026-11-25), on the session bus (step 3). |
| Audio and volume keys | pamixer, pactl, playerctl | none; media keys stay the host's | PulseAudio on CoreAudio (step 3). |
| Idle and lock | swayidle, swaylock | none | Step 8; macOS's own lock still works meanwhile. |
| Screenshots | grimshot (grim, slurp, wl-copy) | none | Step 8. |
| Scale | 1.6 | integer 2 from macOS | Step 4. |

## Step 0: Firefox spike, in the background (start now)

The browser is the biggest risk and a hard requirement, and the build takes over an hour, so it runs in the background while steps 1 and 2 go on in the foreground.

**Firefox with the GTK toolkit on Darwin**: `--enable-default-toolkit=cairo-gtk3-wayland-only` against the Wayland-only GTK 3 already in the `darwinWayland` scope (built for waybar), with software WebRender, which presents through wl_shm when there's no EGL.

- The hard part is Gecko's split between OS and toolkit. `XP_MACOSX` and `XP_DARWIN` guard both what really is Darwin (Mach-port IPC and shared memory, the seatbelt sandbox, CoreAudio through cubeb) and what is Cocoa (`widget/cocoa`, native key bindings, menus). The first must stay; the second has to follow `MOZ_WIDGET_GTK`, the same kind of change as GTK's `003-mac-bindings-by-backend.patch`, in a much bigger tree.
- Start from nixpkgs' `firefox-unwrapped` (it builds for Darwin with Cocoa) overridden in the scope, and carry changes in `patches/firefox/`.
- **Brief for the porting agent: `firefox-port.md`** (fonts: CoreText). **Survey (2026-11-24, `firefox-notes.md`):** keep the OS as OSX and switch the AppKit-meaning sites to `MOZ_WIDGET_COCOA`. That's about 120-150 files and 300-450 patch sites, mostly mechanical, roughly 1-2 weeks to a first link and 1-3 weeks of runtime work. The port is M3's long pole, not a spike. It goes in stages: configure, link, start-up, first window, pages, then Linux shortcuts.
- **Fallback if Firefox stalls:** WebKitGTK (GTK 4 API) with a small front end. WebKit's core already targets Darwin and renders on the CPU with Skia without EGL. It's a fallback for getting a browser into the session, not a Firefox replacement; whether to use it is the owner's call at that point.

Exit: a page loads, scrolls and takes text input with Ctrl shortcuts; a video plays with sound.

## Step 1: frame pacing, damage and power

The day runs on battery, and today every display link tick sends frame callbacks to every surface and redraws the whole output whenever anything commits.

- Frame callbacks only to surfaces that are visible (current workspace, not fully covered), so hidden clients stop drawing.
- `wp_presentation` (Smithay `presentation`) with the display link's timestamps: fixes GTK's `gdk_frame_timings_*` warnings.
- Damage: upload only the damaged region of shm buffers, and skip the frame when nothing changed and no animation runs. Pause the display link when idle.
- Refresh rate: ask `CADisplayLink` for the screen's maximum (`preferredFrameRateRange`). The built-in panel is 60 Hz, so 120 Hz can't be checked on this machine; it stays a "written, not checked" item.

Check: idle CPU and energy in Activity Monitor with a terminal and waybar open; animations still smooth.

## Step 2: clipboard

Copy and paste between Wayland clients already works (`wl_data_device`). Adding:

- `zwp_primary_selection` (select, then middle-click to paste).
- `ext-data-control` / `wlr-data-control`, needed by `wl-copy`/`wl-paste` and grimshot.
- Terminal programs: check how bemtvi's TUI reaches the clipboard inside VTE (OSC 52 or `wl-copy`).
- **No bridge to the macOS pasteboard** (owner, 2026-11-25): the session's clipboard is its own.

Check: middle-click paste between GTK apps and VTE; `wl-copy`/`wl-paste` round trip.

## Step 3: session services: D-Bus, notifications, audio

- **D-Bus session bus** started by the session (nixpkgs' `dbus` builds on Darwin), with `DBUS_SESSION_BUS_ADDRESS` exported to clients. Removes the "Unable to acquire session bus" noise and unlocks notifications, MPRIS (playerctl) and GTK's file-transfer portal registration (`gtk4-notes.md`).
- **dunst** for notifications (owner, 2026-11-25: mako's sd-bus library, basu, has no Darwin build; dunst uses GLib's D-Bus).
- **Audio:** a PulseAudio daemon with its CoreAudio modules (`module-coreaudio-detect`) as the session's sound server, so pamixer, pactl, and GTK/GStreamer apps work unchanged. PipeWire has no CoreAudio backend. Firefox's cubeb can use PulseAudio or CoreAudio directly; prefer PulseAudio so pactl sees it. The volume keys stay macOS's in Stage A (fn/media keys aren't captured), which is fine.

Check: a notification from `notify-send`, a sound from a GStreamer app, `pamixer -i 3` changes the volume.

## Step 4: scaling on the built-in panel

- **`hl.monitor`**: honour `scale` for the one output, with `wp_fractional_scale_v1` and `wp_viewporter` (both in Smithay). GTK 4 draws at fractional scales (verify with the cairo renderer); GTK 3 (waybar, Firefox) draws at 2 and the compositor scales it down, as Hyprland does. Firefox on Wayland supports fractional scale itself (check under software WebRender).
- **Pixel size:** in macOS's scaled display modes, WindowServer resamples the window, so a compositor scale of 1.6 would be resampled twice. Draw at the panel's native pixels: either recommend the macOS mode whose backing equals native pixels (1280x800 @2x on this 2560x1600 panel) or size the drawable to native pixels from the `CGDisplay` mode. Decide while building it.
- Group bar titles (Core Text) and cursors at the output scale.

Check: logical 1600x1000 on the built-in panel with sharp text in GTK 4, waybar and Firefox.

## Step 5: bemtvi

**Terminal mode first (small).** bemtvi's TUI (crossterm, ratatui) in VTE: truecolor, keys (Alt as left Option, the Linux key bindings), mouse, and images. ratatui-image speaks Kitty, Sixel or iTerm2 graphics; VTE has Sixel when built with `-Dsixel=true`, so turn that on in our VTE package.

**GUI (larger, crosses into M4).** `bemtvi-gui` draws with wgpu (and glyphon) onto mitsuami's `GpuSurface`. In mitsuami's GTK backend that's a desynchronised Wayland subsurface (`mitsuami-linux::wayland`) that wgpu presents to. Three pieces of work:

1. **mitsuami on Darwin with the GTK backend.** The backend is chosen by target OS (`cfg(target_os = "linux")` in `mitsuami` and `mitsuami-gtk`; AppKit on macOS). It needs a way to pick GTK on macOS (for example the `gtk` feature taking effect there too, or a new `wayland` feature), and `mitsuami-linux`'s Wayland code to build on Darwin. Also libadwaita in the `darwinWayland` scope. This is a change in the owner's library (a shared library: caret dependencies, its own release).
2. **wgpu can't present to a Wayland surface on Darwin**: its Wayland path is Vulkan or EGL, and neither has a Wayland platform here (MoltenVK/KosmicKrisp have no Wayland WSI; that's M4). An M3 stopgap: on Darwin, `GpuSurface` renders with wgpu's Metal backend into an offscreen texture and copies each frame into a wl_shm buffer. One readback per frame; fine for an editor at 60 Hz.
3. **M4 makes it zero-copy:** with the IOSurface buffer protocol (outline M4), wgpu's Metal backend renders into an IOSurface-backed texture and the compositor samples it directly. bemtvi is a natural first M4 client, ahead of a browser.

The GTK backend on Darwin also serves the planned settings app (`decisions.md`, 2026-11-21).

Check: bemtvi TUI with highlighting, LSP and images in VTE; then `bemtvi-gui` open, typing and scrolling at 60 Hz.

## Step 5b: 2ksbox in the session

The owner wants `~/work/win-98-xp-virt` (2ksbox) working too. Its launcher and player are mitsuami apps (`launcher-mitsuami`, `player-mitsuami`), and the player draws the guest's picture with wgpu (librashader's CRT chain in `shader-chain`) onto a `GpuSurface`, so step 5's mitsuami work carries most of it. What's left on top:

- **Its mitsuami is older:** pinned `=1.3.6`, while bemtvi is on 2.2. The GTK-on-Darwin change lands in a 2.x release, so 2ksbox moves to it first (or the change is backported to 1.3).
- **Its own OS checks:** about 80 `target_os = "macos"`/`"linux"` sites across `player-mitsuami`, `launcher-mitsuami`, `player-core` and `launcher-core`. Some really mean the OS and stay: HVF, CoreAudio, gamepads and QEMU's own build. Others mean AppKit or Mac shortcuts and must follow the backend. For example, `player-mitsuami/src/main.rs:548` picks Cmd over Ctrl, and raw motion is skipped on macOS at line 982. Its keyboard capture through `CGSSetGlobalHotKeyOperatingMode` must not run inside the session, where the compositor owns that.
- **Compositor protocols the player needs:** `keyboard-shortcuts-inhibit` (GTK's `gdk_toplevel_inhibit_system_shortcuts`, so Cmd and the compositor's binds reach the guest while it has the keyboard; Cmd+Shift+Q still leaves), and `pointer-constraints` plus `relative-pointer` for mouse lock in guests without a tablet. Smithay has all three.
- **Frame copies:** the player is the heaviest user of step 5's wl_shm stopgap: a guest picture at up to 60 Hz after the CRT pass, read back each frame. That's acceptable for M3; M4's IOSurface protocol removes it.

Check: the launcher creates and starts a machine; the player shows Windows 98 and XP with the CRT shader, with sound, keyboard (Cmd reaching the guest while captured) and locked mouse; a 3D game runs.

## Step 6: Firefox in the session, and launching apps

- **Firefox:** package the step 0 build: profile in `~/.mozilla` as on Linux (or a session-specific location, to keep it apart from a Mac Firefox profile), `.desktop` file, `MOZ_ENABLE_WAYLAND=1`. Check video, sound, file chooser (GTK's, not a portal), downloads, extensions, sync.
- **Launching apps:** GLib on Darwin has no `GDesktopAppInfo`. Patching GLib means rebuilding everything above it locally (GTK, GStreamer, librsvg and more, because a second GLib in the scope would load two copies in one process). Proposed: don't patch GLib in M3; port the owner's fzf launchers, which read `.desktop` files themselves, and install the session apps' `.desktop` files into one `XDG_DATA_DIRS` tree. Nautilus stays out until GLib is settled (it also wants Tracker). Firefox opening links in other apps (`xdg-open`) needs a small handler.

## Step 7: kiosk presentation, app bundle, launch at login

- **Presentation:** in full screen, hide the menu bar and Dock entirely (`HideMenuBar | HideDock` rather than auto-hide) and keep the existing capture. Trackpad system gestures (Mission Control, App Exposé, swiping between full-screen apps) can't be captured from an app; turn them off with `defaults` (`com.apple.AppleMultitouchTrackpad` and `com.apple.dock` keys), which the nix-darwin module sets in step 9.
- **App bundle:** a real `.app` with a stable bundle id. macOS privacy permissions (microphone, camera, screen recording) are granted to the responsible process, which is the compositor for every client it launches (Firefox's camera and microphone included), and they're tied to the code signature: an ad-hoc signature changes with every Nix rebuild and loses the grants. Sign with a local self-signed certificate so the designated requirement stays the same across builds.
- **Launch at login:** a user LaunchAgent (`LimitLoadToSessionType = Aqua`) that starts the session, relaunching after a crash but not after Cmd+Shift+Q (`KeepAlive = { SuccessfulExit = false }`).
- Lid close, sleep and wake while running: check they're survived.

## Step 8: idle, lock and screenshots (could slip past M3)

- `ext-idle-notify` and `idle-inhibit` (Smithay) for swayidle (Firefox inhibits idle during video); `ext-session-lock` for swaylock, whose PAM needs checking against macOS's OpenPAM.
- `ext-image-copy-capture` (Smithay has the protocol; the capture is a render into the client's buffer) for grim, plus slurp; then grimshot.

## Step 9: nix-darwin module

- `darwinModules.default` in the flake: the session packages, the app bundle in `/Applications`, LaunchAgents for the session, D-Bus and PulseAudio, the environment now set by `nix/pkgs/session.nix`, and the trackpad and Dock defaults from step 7.
- This machine has no nix-darwin today. Test the module in a UTM macOS VM first; installing it on the owner's Mac is their decision.

## Exit check

A working day in the session on the owner's MacBook, with a list of everything that forced a trip to the Mac desktop. That list starts M4 or an M3 follow-up.

## Moved out of M3

- **Qt 6** (no Qt app in daily use). Revisit when one is needed; the Ctrl/Meta key-binding check applies to it as it did to GTK.
- **Multi-monitor** (one compositor window and `wl_output` per `NSScreen`, workspaces bound to monitors). The owner's config has a 144 Hz external monitor, which is also where high refresh rates would be checked.
- **Input methods** (fcitx5 over `input-method-v2`) and the emoji picker.
- GPU-accelerated clients (M4): wezterm, GL/Vulkan apps, GTK's GL renderer, zero-copy bemtvi.
- **Chrome** (owner, 2026-11-24): a later milestone. Chromium's Wayland (Ozone) backend is unsupported on macOS upstream and the build takes many hours, so it's a project the size of the Firefox port or bigger.
- Our own settings app; M2 leftovers (`m2-notes.md`) unless the day needs them.
