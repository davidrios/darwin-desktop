# Decision log

Newest first. Each entry: what was decided, why, and what it implies. Earlier decisions are recorded in `PROJECT_OUTLINE.md` section 2.

## 2026-11-21: Track 2, a new compositor on Smithay

**Decision:** The compositor is new code built on Smithay for the Wayland protocol layer. Cocoa-Way is not forked; it remains a reference to study and a test display, and none of its code is copied.

**Why:** Smithay (MIT) covers the largest and least project-specific part (protocol state for xdg-shell, layer-shell, data device, seat, output and so on). What's specific to this project is ours to write: Metal renderer, IOSurface buffers, AppKit window and input (later IOHID and the framebuffer), Hyprland-style layouts, workspaces, animations, keybinds and config. Not forking keeps the licence choice open (Cocoa-Way is GPL-3.0) and avoids inheriting its container/waypipe product scope.

**Implications:**
- Track 1 (Hyprland/aquamarine port) is no longer the plan; its analysis stays in the outline as the comparison point.
- Language is Rust (Smithay's language). Dependencies are pinned exactly (the compositor is distributed as a binary).
- Any Darwin fixes Smithay needs go upstream where possible, otherwise carry them as patches in the flake.
- Licence of the new code: not decided yet.

## 2026-11-20: apps use Linux shortcuts; Cmd belongs to the compositor

**Decision:** Wayland apps in the session use the standard Linux key bindings (Ctrl+C, Ctrl+V, Ctrl+Z, Ctrl+A). Cmd reaches clients as evdev `KEY_LEFTMETA`/`KEY_RIGHTMETA` (xkb Super), which is free to serve as the compositor's main modifier, the way Super does in Hyprland.

**Why:** "Linux in spirit", and it keeps one modifier for the window manager. The alternative, Mac-style Cmd+C in apps, needs Cmd delivered as xkb Meta and leaves the compositor without a modifier of its own.

**Found while testing GTK under Cocoa-Way:** GTK compiled on Apple turns on Mac-style bindings (`GDK_META_MASK`) by `#ifdef __APPLE__`, independent of the GDK backend, and drops the Ctrl ones. With Cmd sent as Super and Option as Alt, nothing produces Meta, so no copy/paste/undo shortcut worked at all (the menu showed "Meta+Z").

**Implications:**
- `nix/patches/gtk4/mac-bindings-by-backend.patch` switches those blocks (13 files, including Ctrl+click as secondary click in `gdkevents.c`) to `GDK_WINDOWING_MACOS`.
- Qt and other toolkits will need the same check when they are ported.
- The compositor's own keybinds default to Cmd (Super) chords; the Cmd+Shift+Q escape chord above fits this.
- Muscle memory: people coming from macOS will press Cmd+C inside apps. A per-app or global remap (Cmd+C → Ctrl+C) could come later as a compositor option; not decided.

## 2026-11-20: keyboard capture in Stage A follows 2ksbox

**Decision:** In Stage A (the compositor as a full-screen app), system shortcuts are captured the way 2ksbox does it (`~/work/win-98-xp-virt`, `docs/03-display-pipeline.md` "Input path"; implementation in the `mitsuami-appkit` crate, `src/surface.rs`):

- While the compositor's window is key, set the window server's hot key mode to "all disabled except Universal Access" with SkyLight's private `CGSSetGlobalHotKeyOperatingMode(CGSMainConnectionID(), 2)`. Read the previous mode with `CGSGetGlobalHotKeyOperatingMode` first, read it back after setting to confirm it took, and restore it when the window resigns key or the session ends.
- Look the three symbols up with `dlsym(RTLD_DEFAULT, …)` rather than linking them, so a macOS that drops them degrades instead of failing to launch.
- Fallback when the symbols are missing or the mode doesn't take: `NSApplicationPresentationOptions` (`disableProcessSwitching`, `disableHideApplication`, and the kiosk options). These cover far less (no Ctrl+arrows, no Cmd+Space).
- The app's own menu key equivalents (Cmd+Q, Cmd+H, Cmd+Opt+H) are not hot keys: the menu swallows them before the view sees them. The compositor app has no key equivalents on its menu items while capturing.
- On focus loss, release every key clients still see as held (`wl_keyboard.leave`), so a Cmd+Tab can't leave a modifier stuck down in a client.

**Why:** 2ksbox measured the alternatives on macOS 26 (2026-09-24) and found them dead: Carbon `PushSymbolicHotKeyMode` is a stub, and an HID-level `CGEventTap` with Accessibility granted never sees Cmd+Tab, Cmd+Space or Ctrl+Up key downs, because the window server acts on them before any tap. The private call is what UTM and VirtualBox use, works in the App Sandbox, and the user has confirmed it with real keys. Hyprland-style use needs Cmd (as the main modifier), Cmd+Tab, Cmd+Space and Ctrl+arrows to reach the compositor, so the public routes are not enough.

**What stays the host's even when captured:** Universal Access chords, Cmd+Opt+Esc (Force Quit) and the fn / media keys. That's acceptable in Stage A: brightness and volume should keep working through macOS anyway. Stage B (M5) reads IOHID directly and has no window server to fight.

**To verify early (M1):**
- Whether the mode is restored by the window server when the process dies without restoring it (crash, `kill -9`). If not, a crashed compositor leaves the Mac without Cmd+Tab until something resets it; the session launcher must restore the saved mode after the compositor exits.
- Whether Cmd+Shift+Q (Apple menu "Log Out", see below) is covered by the hot key mode or needs the presentation options too.

## 2026-11-20: Cmd+Shift+Q leaves the session

**Decision:** Cmd+Shift+Q is the escape chord that ends the custom desktop session and returns to macOS.

**Why:** It is already the macOS chord for logging out, so the muscle memory carries over and it means the same thing: leave the current session.

**Implications:**
- The compositor handles this chord itself and never forwards it to Wayland clients, regardless of focus or keybind config. Config cannot rebind it away, so there is always a way out. It is the compositor's equivalent of 2ksbox's Ctrl+Alt+K: the chord that gives the host its keyboard back, except here it also ends the session.
- Stage A (nested app, M1–M4): with the keyboard captured (entry above), the chord must reach the compositor as an ordinary key down before macOS acts on it. "Log Out" is an Apple menu item rather than a Keyboard-pane hot key, so the hot key mode may not cover it; the kiosk option `NSApplicationPresentationDisableAppleMenu` should. Verify both. If macOS still sees it, the result is a logout prompt, which is a safe failure.
- Ending the session restores the saved hot key mode before the process exits.
- Stage B (M5, no WindowServer/loginwindow): the chord ends the compositor session; what it returns to (loginwindow restart or a shutdown/restart prompt) is decided in M5.
- Consider requiring a short hold or a confirmation so it can't be hit by accident while typing. Not decided.

## 2026-11-20: patched libwayland lives in a scope, not as a global override

**Decision:** The Darwin build of libwayland is `darwinWayland.wayland`, inside a `lib.makeScope` in `nix/overlay.nix`. Top-level `pkgs.wayland` is untouched.

**Why:** nixpkgs marks `wayland` as a bad platform on Darwin on purpose: if it were available, packages that test `lib.meta.availableOn wayland` (qtbase among others) would switch on Wayland support and break. Overriding it globally would also invalidate the binary cache for everything that depends on it. Packages added to the scope get the patched `wayland` through `callPackage`.

## 2026-11-20: Darwin wayland patch source

**Decision:** Vendor the patch series from `barracuda156/wayland-mac` (branch `1.26.0-mac`, commit `eb50623f`) as `nix/patches/wayland/darwin-1.26.0.patch`, applied to the upstream 1.26.0 release tarball.

**Why:** It is what MacPorts ships, it includes owl-compositor's Darwin fixes, and it is based on the same 1.26.0 that nixpkgs packages. Vendoring the diff keeps the build independent of a personal fork's branch.
