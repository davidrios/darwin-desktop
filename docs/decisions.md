# Decision log

Newest first. Each entry: what was decided, why, and what it implies. Earlier decisions are recorded in `PROJECT_OUTLINE.md` section 2.

## 2026-11-20: Cmd+Shift+Q leaves the session

**Decision:** Cmd+Shift+Q is the escape chord that ends the custom desktop session and returns to macOS.

**Why:** It is already the macOS chord for logging out, so the muscle memory carries over and it means the same thing: leave the current session.

**Implications:**
- The compositor handles this chord itself and never forwards it to Wayland clients, regardless of focus or keybind config. Config cannot rebind it away, so there is always a way out.
- Stage A (nested app, M1–M4): the chord must reach the compositor before macOS acts on it. In kiosk mode the Apple menu is disabled (`NSApplicationPresentationDisableAppleMenu`), so the system "Log Out" shortcut should not fire; verify this early, together with the open question in the outline about capturing system shortcuts. If macOS still sees it, the result is a logout prompt, which is a safe failure.
- Stage B (M5, no WindowServer/loginwindow): the chord ends the compositor session; what it returns to (loginwindow restart or a shutdown/restart prompt) is decided in M5.
- Consider requiring a short hold or a confirmation so it can't be hit by accident while typing. Not decided.

## 2026-11-20: patched libwayland lives in a scope, not as a global override

**Decision:** The Darwin build of libwayland is `darwinWayland.wayland`, inside a `lib.makeScope` in `nix/overlay.nix`. Top-level `pkgs.wayland` is untouched.

**Why:** nixpkgs marks `wayland` as a bad platform on Darwin on purpose: if it were available, packages that test `lib.meta.availableOn wayland` (qtbase among others) would switch on Wayland support and break. Overriding it globally would also invalidate the binary cache for everything that depends on it. Packages added to the scope get the patched `wayland` through `callPackage`.

## 2026-11-20: Darwin wayland patch source

**Decision:** Vendor the patch series from `barracuda156/wayland-mac` (branch `1.26.0-mac`, commit `eb50623f`) as `nix/patches/wayland/darwin-1.26.0.patch`, applied to the upstream 1.26.0 release tarball.

**Why:** It is what MacPorts ships, it includes owl-compositor's Darwin fixes, and it is based on the same 1.26.0 that nixpkgs packages. Vendoring the diff keeps the build independent of a personal fork's branch.
