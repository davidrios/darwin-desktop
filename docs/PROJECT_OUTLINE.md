# Darwin Wayland desktop: project outline

Working title only. Written 2026-11-19 as a handover for a Claude Code session.

## 1. Goal

Run a "Linux in spirit" desktop on Apple Silicon Macs without running Linux:

- Apple's kernel (XNU), kexts, firmware and hardware daemons stay, so every chip generation is supported with Apple's own drivers and power management.
- The desktop is free software: Hyprland as the Wayland compositor, Wayland-native apps, a package manager for the userland.
- The macOS interface (WindowServer, Dock, Finder, AppKit apps) ends up unused and, in the last stage, not running.

Pragmatic, not dogmatic: closed Apple daemons and frameworks (Metal, CoreAudio, Wi-Fi, Bluetooth) are acceptable where they act as drivers.

### Non-goals

- Running Mac apps or Linux binaries.
- A custom-built XNU. The stock kernel is enough; the lock-in is above the kernel.
- GNOME. Mutter, GNOME Shell and systemd are too Linux-bound.
- Staying bound to Hyprland's code or its limitations. Hyprland is the model for behaviour (dynamic tiling, workspaces, animations, config style), not a dependency that must be kept.

## 2. Decisions so far, and what was rejected

| Option | Verdict | Why |
|---|---|---|
| Tweak stock macOS (yabai, AeroSpace, SketchyBar, hyprmac) | Rejected as the goal | Still the Mac desktop underneath. Usable as a stopgap. |
| Asahi Linux | Rejected | Driver support lags new Macs. In Nov 2026 M3 has no GPU acceleration or sleep; M4/M5 are early bring-up. |
| Custom XNU + Apple kexts + fully free userland | Rejected | Half of each "driver" lives in closed userland (Metal driver, WindowServer as display client, Wi-Fi/audio daemons). |
| Linux VM with a full-screen overlay (2ksbox-style: QEMU in-process, virtio-gpu Venus, Vulkan-on-Metal) | Fallback (plan B) | Works with little reverse engineering, but adds a second kernel, duplicated RAM and a long GPU translation chain. No true GPU passthrough on Apple Silicon. |
| **Native Darwin + a Hyprland-style Wayland compositor** | **Chosen** | One kernel, lean memory, Apple drivers. Costs: toolkit and app porting, plus the compositor work below. |

Plan B stays documented because it is the escape hatch if the app-porting cost of the native route proves too high. Reference implementation of its GPU path: steamac (SteamOS VM on Apple Silicon, Venus over MoltenVK/KosmicKrisp).

### Package layer: Nix (decided)

| Option | Verdict | Why |
|---|---|---|
| **Nix (flake + nix-darwin)** | **Chosen** | One flake describes the compositor, patched toolkits, apps and config. A fresh Mac or VM rebuilds to the same state. Overlays carry patched packages without forking nixpkgs. Unpatched packages come from the public binary cache; patched ones build locally, so nothing has to be hosted. |
| MacPorts | Patch source | Best collection of Darwin-specific fixes, and its contributors have worked on Wayland pieces for macOS. Imperative, no rollbacks, hard to reproduce a setup. Fine for a quick throwaway spike. |
| pacman | Deferred | Works on macOS (Homebrew `makepkg` formula, macos-pacman PKGBUILDs), but Arch's binaries are Linux-only, so it means building and hosting a whole repository. Too much work at this stage. |

Known costs of Nix here: Darwin is a second-tier platform in nixpkgs, patching a low-level library rebuilds everything above it locally, and the tooling has a learning curve.

### Compositor strategy: two tracks, decision pending

The latest direction from the project owner is Track 2. Track 1 is kept because its analysis is done and it is the comparison point.

| | Track 1: port Hyprland | Track 2: reimplement in Metal |
|---|---|---|
| What it is | Fork Hyprland and aquamarine; GLES through ANGLE; IOSurface standing in for dmabuf | A new compositor with Hyprland's behaviour, rendering directly with Metal |
| Removes | Nothing; inherits every Linux assumption | ANGLE, EGL, GLES, the dmabuf/GBM emulation, fence sync, libinput shims, the aquamarine fork and all rebasing |
| Gains | Hyprland's features, config format and `hyprctl` for free | Native IOSurface and Metal throughout, native frame pacing, HDR and ProMotion without translation, freedom to change the design |
| Costs | Permanent fork of about 143k lines of fast-moving C++ | Everything Hyprland does must be rebuilt or dropped; no plugin ecosystem; config and IPC compatibility only if written |
| Biggest risk | Rebase burden; ANGLE gaps | Scope creep |

Size of what Track 2 replaces, measured in Hyprland 0.57.0 `src/`: protocols 26k lines (66 protocol implementations), render 19k, config 18k, desktop 17k, managers 15k, layout 10k.

**Recommendation for Track 2: do not rewrite the Wayland protocol layer.** That is the largest and least interesting part. Build on an existing protocol library and write only what is specific to this project:

- Smithay (Rust) is the leading candidate. Cocoa-Way already runs a Smithay compositor on macOS as an ordinary app, which proves the base works. Its README lists Metal rendering; older descriptions say OpenGL, so check the current state. Cocoa-Way is GPL-3.0, so forking it fixes the licence; studying it and starting fresh on Smithay does not.
- New code: Metal renderer (borders, rounding, shadows, blur, animations as Metal shaders), IOSurface buffer handling, Darwin input, Hyprland-style layouts (dwindle, master), workspaces, keybinds, rules.
- Decide early how compatible to be: read a subset of `hyprland.conf`, or define a new config format.

## 3. Architecture

```
            Stage A (M1-M4): nested                Stage B (M5): direct

  Wayland apps (GTK/Qt/terminal/browser, built for Darwin)
        |  wl_shm first, IOSurface-backed buffers later
  Compositor
    Track 1: Hyprland fork, GLES 3 via ANGLE -> Metal,
             aquamarine fork + new Darwin backend
    Track 2: new compositor, protocol library (Smithay) + Metal renderer
        |                                         |
  full-screen Metal/IOSurface window        private framebuffer interface
  input from NSEvent                        input from IOHID
        |                                         |
  WindowServer (hidden behind kiosk mode)   (WindowServer disabled)
        |                                         |
  XNU + Apple kexts + Metal + hardware daemons (unchanged)
```

Stage A runs as an ordinary app with SIP on and is fully reversible. Stage B needs SIP off and reduced boot security.

## 4. Milestones

Each milestone is usable or at least demonstrable on its own.

### M0. Userland baseline on stock macOS

- Set up the project flake: a dev shell first, nix-darwin configuration later (M3).
- Keep all Darwin patches as overlays in the flake. When a package fails on Darwin, look for an existing fix in MacPorts before writing one.
- Build for Darwin arm64: libwayland (needs epoll-shim), wayland-protocols, xkbcommon, pixman, cairo, pango, hyprutils, hyprgraphics, hyprcursor, hyprwayland-scanner, hyprland-protocols.
- **Exit:** a trivial Wayland server and a wl_shm client talk to each other on macOS.

M1 and M2 differ by track. M0, M3, M4 and M5 are the same for both.

### M1 and M2 on Track 2 (Metal reimplementation)

- **M1:** compositor skeleton on the protocol library: full-screen window, Metal renderer, wl_shm clients composited, keyboard and pointer input. **Exit:** one client window on screen, usable.
- **M2:** Hyprland behaviour: dwindle and master layouts, workspaces, focus rules, keybinds from config, animations with bezier curves, borders, rounding and blur as Metal shaders, trackpad gestures, layer-shell for bars and launchers. **Exit:** tiling, workspace switching and animations feel like Hyprland with a terminal and a bar running.

### M1 on Track 1: aquamarine Darwin backend (nested)

- Make aquamarine build on Darwin with DRM, GBM, libseat, libudev and libinput compiled out.
- Add a backend that opens a full-screen window and presents IOSurface-backed buffers.
- Add an IOSurface allocator and buffer type.
- Translate NSEvent keyboard, pointer, scroll and trackpad gesture events into aquamarine input devices.
- **Exit:** `tests/SimpleWindow.cpp` shows frames in a full-screen window and logs input.

### M2 on Track 1: Hyprland on that backend, shm clients only

- EGL through ANGLE's Metal backend; render into IOSurface-backed targets.
- Disable the dmabuf-based protocols; clients use wl_shm.
- Fix the Linux-only code paths listed in section 6.
- **Exit:** Hyprland starts, draws borders and animations, and one wl_shm client is usable with keyboard, pointer and gestures.

### M3. A session you can live in (still an app)

- GTK 4 and Qt 6 Wayland backends building on Darwin, then a terminal, an editor and a browser.
- Kiosk presentation: hide Dock and menu bar, block app switching, capture system gestures and shortcuts.
- HiDPI scaling, 120 Hz frame pacing, clipboard, audio through CoreAudio.
- Launch at login.
- Move from the dev shell to a nix-darwin configuration that installs the whole session.
- **Exit:** a full working day inside the session without touching the Mac desktop.

### M4. Accelerated clients

- Wayland shares GPU buffers through dmabuf, which Darwin lacks. Design a small protocol that passes IOSurface handles instead.
- Client-side support: an EGL/Vulkan window-system layer for "Wayland on Darwin" (ANGLE or KosmicKrisp based).
- **Exit:** a GPU-rendered client (browser or a Vulkan demo) presents without CPU copies.

### M5. Remove WindowServer

- In a macOS VM first: boot with WindowServer disabled and see what breaks (sleep, brightness, display hotplug, daemons that expect a GUI session).
- Second display backend (in aquamarine on Track 1, in the compositor on Track 2) that drives the display through the private framebuffer interface, with input from IOHID. Trace WindowServer on a live system to learn the calls.
- Strip unneeded services; measure memory.
- Session start without loginwindow, power and lid handling, brightness and volume keys.
- **Exit:** the Mac boots straight into Hyprland with WindowServer not running.

M5 is the only part that needs reverse engineering and the only part that can be broken by a macOS update at the kernel-interface level. Everything before it is porting.

## 5. Source map

On Track 1 this is the list of code to change. On Track 2 it is the reference for behaviour to reproduce (layouts in `src/layout/`, animation in `src/animation/`, shaders in `src/render/shaders/`, config in `src/config/`) and for which Wayland protocols real clients expect (`src/protocols/`).

Checked against shallow clones on 2026-11-19: aquamarine `8126552`, Hyprland `9927f2e` (version 0.57.0). Both are BSD 3-Clause.

### aquamarine (about 7k lines in the backends)

| Path | Relevance |
|---|---|
| `include/aquamarine/backend/Backend.hpp` | `IBackendImplementation` is the interface to implement. `eBackendType` currently has WAYLAND, DRM, HEADLESS, NULL; add a Darwin type. |
| `src/backend/Wayland.cpp` (930 lines) | Best template: the existing "render into a window" backend. Note it requires `zwp_linux_dmabuf_v1` and GBM. |
| `src/backend/Headless.cpp` (275), `Null.cpp` (89) | Smallest complete backends; good skeletons. |
| `src/backend/drm/*`, `Session.cpp` | Linux-only (DRM/KMS, libseat, udev, libinput). Compile out on Darwin. |
| `include/aquamarine/allocator/Allocator.hpp`, `src/allocator/GBM.cpp` | `IAllocator` to implement for IOSurface. |
| `include/aquamarine/buffer/Buffer.hpp` | `IBuffer` exposes `dmabuf()` and `shm()`. Needs an IOSurface accessor. |
| `include/aquamarine/input/Input.hpp` | `IKeyboard`, `IPointer`, `ITouch` and friends to feed from NSEvent / IOHID. |
| `src/backend/Backend.cpp` | Uses `timerfd` for idle events (lines 108, 290). CMake already links epoll-shim when timerfd is missing. |
| `CMakeLists.txt` | Hard requirements today: libseat, libinput, libdrm, gbm, libudev, libdisplay-info, hwdata, wayland-client, pixman, hyprutils. |
| `tests/SimpleWindow.cpp` | First thing to get running in M1. |

`IBackendImplementation` has DRM-shaped methods (`drmFD()`, `drmRenderNodeFD()`, `getRenderFormats()` returning `SDRMFormat`). On Darwin return -1 for the fds and keep DRM fourcc codes as the format vocabulary.

### Hyprland (about 143k lines in `src/`)

| Path | Relevance |
|---|---|
| `src/render/OpenGL.cpp:168` `initEGL(bool gbm)` | Picks `EGL_PLATFORM_GBM_KHR` or `EGL_PLATFORM_DEVICE_EXT`. Add an ANGLE/Metal display path. |
| `src/render/OpenGL.cpp:325-327` | Asserts `eglCreateSyncKHR`, `eglDupNativeFenceFDANDROID`, `eglWaitSyncKHR` exist. ANGLE on Metal will likely fail the fence one; relax it. |
| `src/render/OpenGL.cpp:402-410` | Explicit sync is already force-disabled on non-Linux ("BSD" branch). Darwin takes the same branch. |
| `src/render/OpenGL.cpp:740` `createEGLImage` | Imports dmabufs as EGL images. Needs an IOSurface equivalent. |
| `src/render/gl/GLRenderbuffer.cpp:35-45` | Output buffers become render targets via dmabuf, EGL image, renderbuffer. Main place for the IOSurface render-target path. |
| `src/render/gl/GLTexture.cpp` | Client buffer import; dmabuf path to bypass, shm path to keep. |
| `src/render/shaders/glsl/surface.frag` | Requires `GL_OES_EGL_image_external_essl3`. Check that ANGLE exposes it or guard it. |
| `src/protocols/LinuxDMABUF.*`, `MesaDRM.*` | Disable on Darwin until M4. |
| `src/protocols/core/Shm.*` | The client buffer path for M2 and M3. |
| `src/helpers/Drm.cpp` | Includes `linux/dma-buf.h`, `linux/sync_file.h`. |
| `src/helpers/sync/SyncTimeline.cpp` | Already has BSD guards. |
| `src/Compositor.cpp`, `src/keybinds/Manager.cpp` | Include `linux/vt.h` (VT switching). Stub out. |
| `src/init/initHelpers.cpp` | Includes `linux/capability.h`. Stub out. |
| `src/includes.hpp`, `src/hotkeys/Hotkey.cpp` | Include `linux/input-event-codes.h`. Vendor the header; map macOS key codes to evdev codes in the backend. |
| `src/devices/*`, `src/managers/input/InputManager.*` and about 6 more files | Use libinput types directly for device configuration. Needs guards or a shim. |
| `src/config/shared/inotify/ConfigWatcher.cpp` | inotify. CMake already looks for `libinotify`; check it works on Darwin or use kqueue/FSEvents. |
| `/proc` readers | `helpers/SystemInfo.cpp`, `helpers/MiscFunctions.cpp`, `desktop/view/window/Window.cpp`, `debug/crash/CrashReporter.cpp`, `plugins/PluginAPI.cpp`, `plugins/HookSystem.cpp`. Several already have FreeBSD branches. |
| `src/plugins/HookSystem.cpp` | Function hooking with udis86 (x86 disassembler). Disable plugins on arm64 Darwin at first. |
| `src/xwayland/` | Skip; no X11 in scope. |
| `CMakeLists.txt` | Other deps to provide or stub: libeis, gio-2.0, re2, muparser, lcms2, libcanberra, uuid, xcursor, libdrm (headers), gbm. CMake already probes for epoll-shim and libinotify. |

About a dozen files already carry `__FreeBSD__` / `__OpenBSD__` guards. The FreeBSD port is the closest guide to what non-Linux support needs; Darwin adds the missing DRM, evdev and udev layers on top.

Both repos ship an agent instructions file (`AGENTS.md`, and `CLAUDE.MD` in Hyprland). Read them before editing.

## 6. Known technical problems

Items 2 and 3 exist only on Track 1. Item 8 does not apply to Track 2. The rest apply to both.

1. **No dmabuf on Darwin.** It is assumed in aquamarine's windowed backend, in Hyprland's render targets and in client buffer sharing. IOSurface is the replacement. On Track 2 only client buffer sharing remains a problem. wl_shm covers clients until M4.
2. **EGL platform.** Hyprland expects GBM or EGL device platforms from Mesa. On macOS the candidate is ANGLE on Metal, binding IOSurfaces through ANGLE's IOSurface client-buffer extension.
3. **Explicit sync and fences.** Sync-file fences do not exist. Use the existing non-Linux path and remove the hard assert.
4. **Event loop ownership.** The compositor runs its own loop for Wayland clients (epoll in libwayland, so kqueue through epoll-shim; Smithay uses calloop). AppKit wants the main thread's run loop. Decide between running AppKit on the main thread with the compositor on a second thread, or pumping NSEvents from a pollable fd.
5. **Input semantics.** NSEvent gives processed events (accelerated pointer, gesture phases, macOS key codes). Map to evdev key codes for xkbcommon and to aquamarine's swipe/pinch/hold events. Raw multitouch only arrives in M5.
6. **Frame pacing.** Drive frames from the display's refresh callback and report presentation times back, including 120 Hz ProMotion.
7. **Toolkits.** GTK and Qt build with Cocoa backends on macOS by default. The unit of work is "Wayland backend on Darwin" per toolkit, not per app. Browsers and terminals carry their own Wayland code and are separate jobs.
8. **Plugins.** Hyprland's hook system is x86-specific; park it.

## 7. Open questions to verify early

These came from memory or inference and were not checked:

- Track 1: does ANGLE's Metal backend provide everything Hyprland's shaders use (GLES 3.0 is the stated minimum since 0.50; check extensions such as external images and the blur passes)?
- Track 1: exact shape of ANGLE's IOSurface binding (texture target, formats, whether it can be an FBO attachment directly).
- Track 2: does Smithay build and run on current macOS without patches, and which of its backends and helpers assume Linux (DRM, libinput, udev, dmabuf)?
- Is presenting an IOSurface as layer contents fast enough and tear-free at 120 Hz, or is a CAMetalLayer blit needed?
- Which of the M0 packages does nixpkgs already build for Darwin (wayland, epoll-shim, xkbcommon, the hypr* libraries), and which are marked Linux-only?
- Does libwayland build cleanly on current macOS with epoll-shim? A Hyprland maintainer thread from April 2025 says Wayland itself builds on Darwin.
- State of GTK 4 and Qt 6 Wayland backends on Darwin (Owl and MacPorts work from 2024 is the lead).
- Can kiosk mode capture every system gesture and shortcut, or do some need to be disabled in settings?
- M5: does macOS boot and stay stable with WindowServer disabled on Apple Silicon? What does the private framebuffer interface require from its client?
- M5: how low can idle memory go with services stripped? The 3 to 4 GB mentioned earlier was a guess, not a measurement.
- Does the paravirtual GPU in a macOS VM support Metal 4? Relevant only if KosmicKrisp is used in a VM.

## 8. Risks

- **Track 1: permanent fork of a fast-moving project.** A Hyprland maintainer has said Darwin lacks the low-level APIs Hyprland needs. Keep Darwin changes behind guards and in separate files to make rebasing cheap.
- **Track 2: scope.** A compositor is easy to start and slow to finish. Clients depend on many protocols (xdg-shell, layer-shell, data device, text input, fractional scale, screencopy and more). Fix a minimum protocol list per milestone and resist rebuilding all of Hyprland.
- **App ecosystem.** If toolkit Wayland backends on Darwin turn out to be a swamp, the session has nothing to show. M0 and early M3 spikes should test this before heavy investment in M2.
- **M5 fragility.** Private interfaces can change with any macOS update, and it needs SIP off and reduced boot security, which weakens the machine's protection.
- **Licensing.** Apple's licence restricts modifying and redistributing its binaries. Treat M5 as a personal experiment and ship only your own code. Not legal advice.
- **Testing loop.** Hardware-facing work needs a real Mac and manual test runs. Use a macOS VM (UTM) for everything it can cover.

## 9. Development setup

- Apple Silicon Mac, macOS 26 or later (Metal 4, needed if KosmicKrisp is used later).
- Stages M0 to M4: stock macOS, SIP on.
- M5 experiments: UTM macOS guest first (duplicate the VM file before risky changes), then a spare volume or machine.
- Suggested repo layout:

```
/compositor      Track 2: the new Metal compositor
/aquamarine      Track 1 only: fork, branch darwin
/Hyprland        Track 1: fork, branch darwin. Track 2: read-only reference checkout
/flake.nix       dev shell, overlays, later the nix-darwin system configuration
/nix             overlays and package definitions for the Darwin userland
/protocols       IOSurface buffer-sharing protocol (M4)
/docs            this file, decision log, reverse-engineering notes (M5)
```

## 10. First session checklist for Claude Code

Common to both tracks:

1. M0 spike: create the flake with a dev shell that provides libwayland + epoll-shim on the Mac, and run a server/client pair. Record what needed an overlay.
2. Test whether a GTK 4 app can be built with the Wayland backend on Darwin and shown in an existing macOS Wayland compositor (Cocoa-Way or Owl). This is the go/no-go for the app ecosystem and does not depend on the compositor choice.

Track 2 (current direction):

3. Build and run Cocoa-Way; read how it wires Smithay, Metal and AppKit input together. Decide: build on Smithay fresh, fork Cocoa-Way (GPL-3.0), or use another protocol library.
4. Skeleton compositor: full-screen window, Metal render loop tied to display refresh, one wl_shm client drawn as a textured quad, keyboard and pointer forwarded.
5. Write the minimum protocol list for M1 and M2, using Hyprland's `src/protocols/` as the checklist of what clients expect.
6. Port one layout (dwindle) and the animation curve logic from Hyprland as the first behaviour test.

Track 1 (if chosen instead):

3. Fork and clone aquamarine and Hyprland; read their `AGENTS.md` / `CLAUDE.MD`.
4. Try configuring aquamarine on Darwin and list every failure; add a CMake option that drops DRM, GBM, libseat, libudev and libinput.
5. Spike outside aquamarine: a small program that creates an ANGLE/Metal GLES 3 context, renders into an IOSurface and shows it in a full-screen window. This de-risks the first three questions of section 7.
6. Write the Darwin backend skeleton from `Headless.cpp`, then wire the spike into it and run `tests/SimpleWindow.cpp`.

## 11. References

- aquamarine: https://github.com/hyprwm/aquamarine
- Hyprland: https://github.com/hyprwm/Hyprland
- Hyprland 0.50 notes (GLES 3.0 minimum): https://hypr.land/news/update50/
- KosmicKrisp (Vulkan on Metal, in Mesa): https://docs.mesa3d.org/drivers/kosmickrisp.html
- Cocoa-Way (Smithay-based Wayland compositor running as a Mac app): https://github.com/J-x-Z/cocoa-way
- Owl (Cocoa Wayland compositor): https://github.com/owl-compositor/owl
- Wawona (feasibility study, Wayland on Apple platforms): https://dc.ewu.edu/srcw_2026/ps_2026/p2_2026/31
- Darnix (Nix OS on a Darwin kernel, proposal): https://discourse.nixos.org/t/darnix-toward-a-nix-os-on-a-darwin-kernel/77779
- PureDarwin: https://github.com/PureDarwin
- macos-pacman PKGBUILDs (reference if pacman is revisited): https://www.github.com/macos-pacman/core
- hyprmac (Hyprland-style tiling on stock macOS): https://alternativeto.net/software/hyprmac/about/
- Plan B references: 2ksbox https://github.com/Roboport-Tecnologia/2ksbox, steamac https://github.com/fxgl/steamac/wiki, QEMU Venus-on-macOS RFC https://patchew.org/QEMU/20251203040754.94487-1-j@getutm.app
