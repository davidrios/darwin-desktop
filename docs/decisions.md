# Decision log

Newest first. Each entry: what was decided, why, and what it implies. Earlier decisions are recorded in `PROJECT_OUTLINE.md` section 2.

## 2026-11-25: dunst for notifications

**Decision (owner):** The session's notification daemon is dunst, not mako (which the owner's Linux config starts).

**Why:** mako talks to D-Bus through an sd-bus library, and the usable one (basu) has no Darwin build: porting it means Linux peer credentials, ELF section tricks, endian and locale headers. dunst uses GLib's D-Bus, which already works here, and needed a three-line patch.

**Implications:** dunst starts on demand through D-Bus activation, so the config needs no exec line; the owner's `hl.exec_cmd("mako")` finds nothing and can go. dunst reads `~/.config/dunst/dunstrc`, and uses built-in defaults without one.

## 2026-11-25: no clipboard sharing with macOS

**Decision (owner):** The session's clipboard is Wayland's own: between clients, plus primary selection and data-control for tools like `wl-copy`. There is no bridge to the macOS pasteboard.

**Why:** The session is meant to replace the Mac desktop, not sit beside it, and M5 has no pasteboard at all.

## 2026-11-24: Firefox on Darwin keeps the OS as OSX, uses the GTK toolkit and CoreText fonts

**Decision:** Firefox is ported with `target.os` left as OSX and the GTK 3 Wayland-only toolkit. `XP_DARWIN`/`XP_MACOSX` keep meaning the Darwin OS and its non-AppKit frameworks, and sites that mean AppKit move to `MOZ_WIDGET_COCOA`. Page fonts stay on CoreText (the Mac font path in gfx/2d and WebRender) rather than fontconfig. Rendering is software WebRender through wl_shm until M4. The brief is `firefox-port.md`; the survey is `firefox-notes.md`.

**Why:** Leaving the OS alone keeps Darwin code that works (Mach IPC, CoreAudio, jemalloc zones) and limits the patches to toolkit guards. CoreText needs about 6 overridden methods and no Rust changes, where fontconfig would need about 25 C++ sites and 8 Rust files, each carried through every Firefox release. The cost is that GTK's own UI finds fonts through fontconfig while page content uses CoreText.

## 2026-11-24: M3 scope

**Decision (owner):** Firefox is the browser M3 must bring into the session, and a Mac browser beside the session doesn't count. The editor is bemtvi (terminal mode first, its mitsuami GUI as the goal), and 2ksbox (`~/work/win-98-xp-virt`, also on mitsuami) should run in the session too. Qt 6, multi-monitor and input methods move out of M3. Chrome waits for a later milestone.

**Why:** M3's exit is a working day without the Mac desktop, so it's scoped to what the owner uses every day. Qt has no daily app to justify building it, and work happens on the built-in panel for now.

**Implications:** The plan is in `m3-plan.md`. The Firefox port starts first, in the background, because it's the biggest risk. mitsuami gains a way to use its GTK backend on macOS, and wgpu reaches the compositor through wl_shm copies until M4's IOSurface protocol.

## 2026-11-21: keyboard layouts generated from macOS; input methods via fcitx5 later

**Decision:**
- The xkb keymap sent to clients is generated from the macOS keyboard layout (`compositor/src/layout.rs`): each key through `UCKeyTranslate` with no modifier, Shift, Option and Option+Shift as xkb levels 1-4, macOS dead keys as xkb dead keysyms (clients' compose tables combine them). It follows the current macOS input source and switches when it changes; `--keyboard-layout <id>` fixes one. Our own config will choose layouts once there is a config.
- **Left Option is Alt** (app shortcuts, menu mnemonics); **right Option is the level-3 shift** that types the layout's Option characters and dead keys.
- **No bridge to macOS input methods.** They draw candidate windows through WindowServer and AppKit and would stop working in M5. Input methods (CJK, emoji) come from fcitx5 ported to Darwin, over Wayland's `input-method-v2` (which Smithay supports), in M3 together with D-Bus.
- Configuration that System Settings does today (layouts, input methods, displays, keybinds) will be our own settings app, a Wayland client in the session (possibly built with mitsuami's GTK backend).

**Why:** Generating from the Mac layout makes typing identical to macOS, Option characters included, with no table of layout names to maintain. `UCKeyTranslate` lives in CoreServices; reading layouts through the Text Input Sources API may need revisiting for M5 (the `.keylayout` data can be read directly).

**Verified (2026-11-21, by hand with GTK):** Mac U.S. Option characters and Option dead keys, Brazilian ABNT2 dead keys and ç, live switching between input sources, left Option as Alt.

## 2026-11-21: licence MIT OR Apache-2.0

**Decision:** This project's own code is dual-licensed MIT OR Apache-2.0, as is usual in the Rust ecosystem (`LICENSE-MIT`, `LICENSE-APACHE`). Third-party code keeps its own licence: patches in `patches/` follow their upstream's.

## 2026-11-21: third-party changes are carried as patch queues

**Decision:** Every change to someone else's code lives in `patches/<project>/NNN-*.patch`, documented in `patches/README.md` (what, why, drop when), the way 2ksbox carries QEMU. Rust dependencies we patch (Smithay, reis) are pinned, pristine submodules in `third_party/`; `scripts/prepare-third-party.sh` exports each pinned commit to `build/third_party/<name>` and applies the queue there, and Cargo uses those copies through path dependencies. The Nix package does the same from the flake source (`inputs.self.submodules = true`). C libraries from nixpkgs keep being patched in their Nix derivations, from the same `patches/` tree.

**Why:** One place to see every divergence from upstream and when it can go. Deterministic prepare (restore, then apply in order) means a half-applied tree can't slip through. No public forks are needed; upstream PRs remain possible and drop a patch when merged.

**Implications:**
- After cloning: `git submodule update --init && scripts/prepare-third-party.sh`.
- `nix build .#compositor` (the flake's default package) builds it the same way.

## 2026-11-21: Track 2, a new compositor on Smithay

**Decision:** The compositor is new code built on Smithay for the Wayland protocol layer. Cocoa-Way is not forked; it remains a reference to study and a test display, and none of its code is copied.

**Why:** Smithay (MIT) covers the largest and least project-specific part (protocol state for xdg-shell, layer-shell, data device, seat, output and so on). What's specific to this project is ours to write: Metal renderer, IOSurface buffers, AppKit window and input (later IOHID and the framebuffer), Hyprland-style layouts, workspaces, animations, keybinds and config. Not forking keeps the licence choice open (Cocoa-Way is GPL-3.0) and avoids inheriting its container/waypipe product scope.

**Implications:**
- Track 1 (Hyprland/aquamarine port) is no longer the plan; its analysis stays in the outline as the comparison point.
- Language is Rust (Smithay's language). Dependencies are pinned exactly (the compositor is distributed as a binary).
- Any Darwin fixes Smithay needs go upstream where possible, otherwise carry them as patches in the flake.
- Licence of the new code: MIT OR Apache-2.0 (entry above).

## 2026-11-20: apps use Linux shortcuts; Cmd belongs to the compositor

**Decision:** Wayland apps in the session use the standard Linux key bindings (Ctrl+C, Ctrl+V, Ctrl+Z, Ctrl+A). Cmd reaches clients as evdev `KEY_LEFTMETA`/`KEY_RIGHTMETA` (xkb Super), which is free to serve as the compositor's main modifier, the way Super does in Hyprland.

**Why:** "Linux in spirit", and it keeps one modifier for the window manager. The alternative, Mac-style Cmd+C in apps, needs Cmd delivered as xkb Meta and leaves the compositor without a modifier of its own.

**Found while testing GTK under Cocoa-Way:** GTK compiled on Apple turns on Mac-style bindings (`GDK_META_MASK`) by `#ifdef __APPLE__`, independent of the GDK backend, and drops the Ctrl ones. With Cmd sent as Super and Option as Alt, nothing produces Meta, so no copy/paste/undo shortcut worked at all (the menu showed "Meta+Z").

**Implications:**
- `patches/gtk4/003-mac-bindings-by-backend.patch` switches those blocks (13 files, including Ctrl+click as secondary click in `gdkevents.c`) to `GDK_WINDOWING_MACOS`.
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

**Verified (2026-11-21, `compositor/src/capture.rs`):**
- The window server restores the mode when the process that set it exits, even after `kill -9` or an exit without restoring. A crashed compositor can't leave the Mac without Cmd+Tab; no launcher-side restore is needed.
- With capture on, Cmd+Tab, Cmd+Space, Cmd+Shift+3/4 and Ctrl+arrows reach clients as key events (checked by hand, key log). Capture is released when the window loses key status and taken again when it regains it.
- Cmd+Shift+Q reaches the compositor's key monitor before any menu, so it quits without a logout prompt; the Apple menu option isn't needed for it.

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

**Decision:** Vendor the patch series from `barracuda156/wayland-mac` (branch `1.26.0-mac`, commit `eb50623f`) as `patches/wayland/001-darwin.patch`, applied to the upstream 1.26.0 release tarball.

**Why:** It is what MacPorts ships, it includes owl-compositor's Darwin fixes, and it is based on the same 1.26.0 that nixpkgs packages. Vendoring the diff keeps the build independent of a personal fork's branch.
