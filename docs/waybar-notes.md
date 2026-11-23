# Waybar on Darwin

The project owner's bar on Linux. Waybar 0.16.0 is a GTK 3 app (gtkmm 3, gtk-layer-shell), so it needs a second toolkit build next to GTK 4.

## Result (2026-11-22): builds, not yet run under the compositor

`nix build .#waybar` builds waybar 0.16.0 for aarch64-darwin against our Wayland GTK 3. Waybar's own unit tests pass in the build, and so does nixpkgs' install check (`waybar --version`). With no compositor running it exits cleanly ("cannot open display"). The binary links only `gtk3-wayland`, our patched libwayland and gtk-layer-shell; nixpkgs' Quartz GTK 3 isn't in its closure. It hasn't drawn a bar yet: that needs the compositor's wlr-layer-shell support.

## Packages (all in the `darwinWayland` scope)

| Package | Change from nixpkgs |
|---|---|
| `gtk3` (`nix/pkgs/gtk3.nix`) | Wayland on; X11, Xinerama, Broadway and Quartz off. Removes upstream's "no Wayland on Darwin" line in `meson.build` (nixpkgs' own postPatch already drops the X11 half). Patch `gtk3/001-no-gdesktopappinfo-on-darwin.patch`. Adds `linux-input-headers`, and `-DWL_EGL_PLATFORM` (below). |
| `gtkmm3` | nixpkgs' gtkmm3 on our GTK 3 and libepoxy. |
| `gtk-layer-shell` | nixpkgs' 0.10.1 on our GTK 3 and libwayland, allowed on Darwin, docs off. Its `dev` output has `gtk-layer-demo`: a small app for checking the compositor's layer-shell support by hand (anchors, margins, exclusive zone, keyboard modes). |
| `waybar` (`nix/pkgs/waybar.nix`) | nixpkgs' 0.16.0 with Linux-only features off (below), allowed on Darwin, patches `waybar/001-darwin.patch` and `002-desktop-app-info.patch`, plus Howard Hinnant's `date` library. Not wrapped with `wrapGAppsHook3`: the hook pulls in nixpkgs' Quartz GTK 3, and the session provides the GTK environment for every app anyway. |

GTK 3 builds locally in about 10 minutes on an M1; gtkmm, gtk-layer-shell and waybar take a few minutes more. Everything else comes from the binary cache.

## Problems found and fixed

1. **GTK 3's `meson.build` forces Wayland off on Darwin.** One-line removal, as for GTK 4.
2. **No `<gio/gdesktopappinfo.h>` on Darwin** (GLib uses `GOsxAppInfo` there). GTK 3's Wayland launch context includes the header without using it: the include is guarded.
3. **`EGLNativeDisplayType` is an `int` on Apple** in Khronos' `<EGL/eglplatform.h>`, and GTK 3's `gdkglcontext-wayland.c` casts a `wl_display *` to it for `eglGetDisplay`. That's a hard error with clang. `-DWL_EGL_PLATFORM` selects the header's Wayland types. GTK 4 doesn't hit this because it uses `eglGetPlatformDisplay`. GL still isn't available at run time (no libEGL until M4): GTK 3 draws with cairo on wl_shm.
4. **Waybar uses `pipe2`, `SOCK_CLOEXEC` and `MSG_NOSIGNAL`**, which Darwin doesn't have. A compat header provides `pipe2` from `pipe` and `fcntl` (not atomic, which only matters if another thread forks in between), and defines `SOCK_CLOEXEC` and `MSG_NOSIGNAL` as 0. With no `MSG_NOSIGNAL`, a write to a closed Sway IPC socket could raise SIGPIPE; the Sway modules aren't useful here anyway.
5. **Darwin's `WEXITSTATUS` takes its argument's address**, so `custom_graph.cpp`'s `WEXITSTATUS(close(...))` doesn't compile. The status now goes into a variable first.
6. **The module factory always refers to the `cffi` module**, but `meson.build` only builds it on Linux and the BSDs. It's built on Darwin too; it only needs `dlopen`.
7. **Waybar's icon lookup is built on `Gio::DesktopAppInfo`**, which giomm doesn't have on Darwin (same GLib gap as item 2). The icon lookup is used by `wlr/taskbar` and by Hyprland workspace window icons. `002-desktop-app-info.patch` adds a small stand-in class: it reads `Name`, `Icon` and `StartupWMClass` from a desktop entry with `Glib::KeyFile` and finds entries by exact name in the XDG data dirs, as upstream's first lookup does. GLib's fuzzy search of all installed entries (`g_desktop_app_info_search`) finds nothing, so an app whose id doesn't match its desktop file name gets the generic icon. Waybar's own icon-loader test passes with the stand-in. Building GLib with its Unix desktop app info on Darwin, an M3 item already, would make the stand-in unnecessary.
8. **No `std::chrono` time zones in this libc++.** Without them, waybar falls back to its `simpleclock` module, which no longer compiles with fmt 12 (`fmt::localtime` is gone). With the `date` library (`howard-hinnant-date`, using the system tz database) waybar builds its real clock module, with time zones and calendar tooltips.

## What's in, what's off

Off (Linux-only libraries or interfaces): PulseAudio, PipeWire (and the privacy module), WirePlumber, JACK, sndio and cava (audio); libinput, libevdev and udev (keyboard-state, backlight); libnl (network); UPower, logind (gamemode, inhibitor) and systemd; MPD, MPRIS, GPS, rfkill (bluetooth), WWAN; the tray (needs D-Bus and libdbusmenu-gtk3); the niri, mango and umbriel IPC modules; man pages. Battery, cpu, memory and the other sysfs or `sysctl` modules are only built on Linux and the BSDs; Darwin versions would be new code.

Built: clock, custom, custom-graph, cffi, image, disk, load, temperature (reads Linux sysfs paths by default, so it needs an explicit `hwmon-path` or shows nothing), user, idle_inhibitor, the `wlr/taskbar` and `ext/workspaces` Wayland modules, and the Hyprland, Sway, River, dwl and Wayfire modules.

**The Hyprland modules need Hyprland's IPC.** `hyprland/workspaces`, `hyprland/window`, `hyprland/language` and `hyprland/submap` talk to `$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock` (requests) and `.socket2.sock` (events). The compositor serves a subset of them (`docs/m2-notes.md`, Hyprland IPC): enough for workspaces, window title and language.

## Runtime notes

- The session launcher sets up GTK 3 too (schemas, Adwaita and hicolor icons, `gtk-3.0/settings.ini`) and ships a default waybar config (`nix/pkgs/session.nix`).
- `util::command::setInheritedFdsCloexec`, run in each child that custom modules spawn, loops `fcntl` over every descriptor up to `sysconf(_SC_OPEN_MAX)` on Darwin (no `close_range` there). With a very high `RLIMIT_NOFILE` that could make spawning slow (compare VTE's crash, `patches/vte/001`).
- No D-Bus session bus yet: waybar's portal lookup (for the colour scheme) can't reach a portal. Not checked how it reports that.
