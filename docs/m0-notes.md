# M0 notes: userland baseline

## Status (2026-11-20)

**Exit criterion met outside Nix:** a Wayland server and a wl_shm client talk on macOS 26.7.3 (arm64). The client fills a POSIX shm buffer, the server maps the passed fd and sees the same checksum. Spike: `spikes/m0-wayland/`.

Built by hand (Homebrew meson/ninja/cmake, macOS SDK libffi/expat, epoll-shim 0.0.20240608 from source) because Nix was not installed yet. The Nix build (`nix flake check`) is written but not yet run.

## Package availability in nixpkgs (nixpkgs-unstable, checked 2026-11-20)

| Package | Darwin in nixpkgs | Notes |
|---|---|---|
| wayland 1.26.0 | No: `badPlatforms = darwin` | Deliberate; see `decisions.md`. Patched build in `nix/pkgs/wayland.nix`. Already pulls epoll-shim on non-Linux. |
| wayland-scanner | Yes (unix) | |
| wayland-protocols 1.50 | Yes (all) | |
| epoll-shim 0.0.20240608 | Yes | Its own tests are disabled on Darwin in nixpkgs (upstream issue 41). |
| libinotify-kqueue | Yes | For Hyprland's config watcher (Track 1) if needed. |
| pixman, cairo, pango | Yes | cairo/pango without Wayland involvement anyway. |
| libxkbcommon | Not checked yet | Defined in all-packages as `libxkbcommon_8`. |
| hyprutils, hyprgraphics | No: linux + freebsd | Probably easy: FreeBSD already allowed. |
| hyprcursor, hyprwayland-scanner, hyprland-protocols | No: linux only | hyprland-protocols is just XML; hyprwayland-scanner is a code generator. |

The hypr* libraries matter mainly for Track 1. On Track 2 only hyprland-protocols (XML) may be useful.

## libwayland on Darwin

- Patch applies cleanly to the 1.26.0 tarball (no fuzz).
- Test suite: 26/26 suites pass after disabling one test (below).
- **Known issue:** `display-test`'s `send_overflow_disconnection` sets `SO_SNDBUF` to 16 KiB and floods a server that is not reading, expecting `EAGAIN`/`E2BIG` within ~4k requests. On Darwin it crawls (about 2.5k iterations in 30 s) and never errors before the timeout. It covers a slow or stalled peer, which matters for a real compositor (a stuck client must not stall it). Unregistered in the Nix build for now; to investigate later (Darwin unix-socket buffer accounting differs from Linux).
- **Observation:** when the client disconnects, the server logs `failed to read client connection` (a read error rather than a clean EOF). Harmless in the spike; likely Darwin returning `ECONNRESET` where Linux returns 0. Check before M1 so it doesn't spam logs.

## Pitfalls found

- **XQuartz ships an old epoll-shim** in `/opt/X11` whose `sys/timerfd.h` defines `struct itimerspec`, clashing with libwayland. Homebrew's pkg-config searches `/opt/X11`. Nix builds are isolated from this; manual builds must set `PKG_CONFIG_LIBDIR`.
- **epoll-shim installs its `.pc` under `libdata/pkgconfig`** (BSD layout) when built by hand.
- **`/usr/bin/env` strips `DYLD_*`** (SIP), so `#!/usr/bin/env bash` scripts lose `DYLD_LIBRARY_PATH`. Use rpaths.
- **MacPorts on this machine is broken** (installed for macOS 15; needs `sudo port migrate`).
- Darwin has no `memfd_create`; clients use `shm_open` + `shm_unlink` + `ftruncate`. Shm object names are limited to 31 characters.

## Next

1. Install Nix, run `nix flake check` and `nix develop`; record anything that differs from the manual build.
2. GTK 4 Wayland backend on Darwin: the go/no-go test for apps (outline section 10, item 2).
3. Cocoa-Way: build, run and read it (Track 2).
