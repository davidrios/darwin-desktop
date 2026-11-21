# GTK 4 on Darwin with the Wayland backend: go/no-go for apps

Outline section 10, item 2.

## Result (2026-11-20): go

GTK 4.24.0 builds on aarch64-darwin with only the Wayland backend, and `gtk4-widget-factory` runs as a Wayland client under Cocoa-Way (a Smithay compositor drawing with Metal). The compositor's debug snapshot shows one live toplevel committing at about 60 frames per second (animated widgets), with frames submitted to its Metal renderer. Confirmed by hand: widgets draw correctly, typing and clicks work, menus and popovers open. Shortcuts needed a further patch (see "Key bindings").

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

## Key bindings: Linux-style (decided 2026-11-20)

GTK enables Mac-style bindings (Meta+A/C/Z, Option word movement, `<Meta>question`, Ctrl+click as secondary click) under `#ifdef __APPLE__`, independent of the GDK backend, and drops the Ctrl bindings there. Tested under Cocoa-Way, which sends Cmd as Super and Option as Alt: nothing produced Meta, so no shortcut worked (the menu showed "Meta+Z"). Patched to `GDK_WINDOWING_MACOS` (`mac-bindings-by-backend.patch`, 31 blocks in 13 files), so apps get Ctrl+C/Ctrl+Z. See `decisions.md`. Confirmed by hand after the rebuild: Ctrl+C/V/Z work and the menu shows "Ctrl+Z".

Left as `__APPLE__` on purpose: `gdkvulkancontext.c` and `gsk/gl/fp16.c` (platform/compiler details), and the file-transfer portal registration in `gdkcontent(de)serializer.c` (it needs D-Bus, not a backend; revisit in M3 with the session bus).

## Testing by hand (2026-11-20)

The user ran `gtk4-widget-factory` under Cocoa-Way for about five minutes: widgets drew correctly, text appeared in entries, clicks worked, popovers and menus opened and closed (about 15 popup surfaces). Shortcuts didn't work before the bindings patch (above).

GTK warned twice about frame timing: `gdk_frame_timings_submitted() called on submitted frame` and `gdk_frame_timings_throttling_hint: runtime check failed (self->result != GDK_FRAME_PREPARING)`. GTK's frame clock and Cocoa-Way's frame callback / presentation feedback disagree. Not fatal; relevant to our compositor's frame pacing (outline section 6, item 6).

## Known issue: print preview crashes

Print preview in `gtk4-widget-factory` aborts. GTK launches the external previewer with `g_app_info_create_from_commandline()`, which GLib on Darwin doesn't support ("Creating an app info from a command line not currently supported", the same missing `GDesktopAppInfo` as above). The failed preview then reaches `preview_print_idle_done` (`gtk/print/gtkprintoperation.c`), which calls `cairo_surface_destroy` on a surface that `gtk_print_operation_unix_launch_preview` has already destroyed, and cairo's reference-count assertion aborts. The same code is on GTK main; whether Linux can reach it hasn't been checked. Fixing GLib's desktop app info on Darwin (an M3 item anyway) should remove the trigger; the extra destroy may also be worth reporting upstream.
