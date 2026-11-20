# GTK 4 on Darwin with the Wayland backend: go/no-go for apps

Outline section 10, item 2.

## Result (2026-11-20): go

GTK 4.24.0 builds on aarch64-darwin with only the Wayland backend, and `gtk4-widget-factory` runs as a Wayland client under Cocoa-Way (a Smithay compositor drawing with Metal). The compositor's debug snapshot shows one live toplevel committing at about 60 frames per second (animated widgets), with frames submitted to its Metal renderer. Visual check and input: to be confirmed by hand (no Screen Recording permission for an automated screenshot).

What it took was small: three source changes to GTK, one upstream patch to libepoxy, and runtime environment variables. No change to GTK's Wayland code beyond the Darwin build guards.

## Packages (all in the `darwinWayland` scope)

| Package | Change from nixpkgs |
|---|---|
| `gtk4` (`nix/pkgs/gtk4.nix`) | `waylandSupport = true`, X11 and Vulkan off, `macos-backend=false`. Removes upstream's "no Wayland on Darwin" line in `meson.build`. Patches: `no-gdesktopappinfo-on-darwin.patch`, `color-picker-by-backend.patch`. Adds `linux-input-headers`. |
| `libepoxy` (`nix/pkgs/libepoxy.nix`) | `-Degl=yes` plus upstream commit 47d975d (EGL on Apple, loads `libEGL.dylib`), with Khronos headers from `libglvnd.dev`, propagated. |
| `linux-input-headers` | `linux/input-event-codes.h` (Linux 6.12) and a `linux/input.h` shim with constants only. |
| `gtk4-demos` | GTK's demo programs wrapped with the runtime environment below. |

Only libwayland, libepoxy, GTK and the wrappers build locally; GLib, Pango, cairo, GStreamer and the rest come from the binary cache. A GTK build takes about 10 minutes on an M1.

## Problems found and fixed

1. **GTK's `meson.build` forces Wayland off on Darwin.** One-line removal.
2. **epoxy 1.5.10 hard-codes EGL off on Apple.** Fixed upstream (2025-10-29), not yet released; vendored. Its public `<epoxy/egl.h>` includes `<EGL/eglplatform.h>`, so the Khronos headers must be propagated.
3. **GLib on Darwin has no `GDesktopAppInfo`** (it uses `GOsxAppInfo`). GTK's Wayland launch context uses it only for a Flatpak app-id lookup; guarded out. **Matters for M3:** app launchers and `gtk4-launch` that rely on `.desktop` files through GIO won't find any on Darwin. Options later: build GLib with its Unix desktop app info on Darwin, or use launchers that parse `.desktop` files themselves.
4. **`gtkcolorpicker.c` chooses the Quartz picker by `__APPLE__`** rather than by GDK backend, so it fails to link without the macOS backend. Patched to use `GDK_WINDOWING_MACOS`; a candidate for upstreaming.
5. **No `linux/input.h`.** GTK's Wayland seat code needs the evdev `BTN_*` constants. Vendored header.
6. **No `libGL` attribute on Darwin in nixpkgs.** Passed as `null`; GTK's Wayland backend only needs EGL headers at build time.

## Runtime environment (set by the `gtk4-demos` wrappers)

- `GSETTINGS_SCHEMA_DIR`: GTK's own schemas.
- `GDK_PIXBUF_MODULE_FILE`: librsvg's loader cache. Without it GTK aborts with "Resource path … is not a valid image" on its SVG icons.
- `GST_PLUGIN_SYSTEM_PATH_1_0`: gst-plugins-base. Without `playbin3` the widget factory aborts.

On Linux nixpkgs' `wrapGAppsHook` sets these; it isn't applied on Darwin. M3's nix-darwin session should provide them session-wide (or the hook should be made to work) rather than wrapping every app.

Also seen, not fatal: "Unable to acquire session bus: … without a machine-id". There is no D-Bus session bus; portals, notifications and anything else on D-Bus need one. M3 item.

## Rendering

No libEGL, so GTK's GL renderer is unavailable and GSK uses cairo with wl_shm buffers. That's the expected path until M4. nixpkgs has ANGLE for Darwin (`angle`), which provides `libEGL.dylib`/GLES on Metal; epoxy would load it, but ANGLE has no Wayland EGL platform, so GTK would still fail EGL init and fall back. A Wayland-on-Darwin EGL platform is M4 work.

## Open: macOS-style key bindings in GTK

GTK enables Mac-style bindings (Cmd+A, Cmd+C, Cmd+Z, word movement with Option, `<Meta>question` for help) under `#ifdef __APPLE__`, independent of the GDK backend: `gtklabel.c`, `gtktext.c`, `gtktextview.c`, `gtklistbox.c`, `gtklistbase.c`, `gtklistfactorywidget.c`, `gtkcalendar.c`, `gtksearchentry.c`, `gtkapplication.c`. They test `GDK_META_MASK`.

In this build those bindings are active, but whether they fire depends on how the compositor maps the Mac keys: if Cmd becomes evdev `KEY_LEFTMETA` (xkb Super), GDK reports `GDK_SUPER_MASK`, and Meta is usually tied to Alt in xkb's default keymap, so "Meta+C" could end up meaning Option+C. Decide the model first (see `decisions.md` once decided), then either keep these blocks (Mac-style editing in apps) or switch them to `GDK_WINDOWING_MACOS` so apps use the Linux bindings (Ctrl+C).
