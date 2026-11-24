# Firefox on Darwin with the GTK 3 Wayland toolkit

M3 step 0 (`m3-plan.md`). Survey of Firefox 160.0.1 source (2026-11-24), read only, nothing built yet. Paths are relative to the Firefox source tree. Spot-checked by hand: the pkg-config switch, the key-table chain order and the wl_shm handle.

## TL;DR

- Keep `target.os == OSX`, so XP_DARWIN, XP_MACOSX, Mach IPC, jemalloc zones, Keychain and CoreAudio all keep working.
- Treat the port as "OSX + toolkit=gtk". Change only the sites where `XP_MACOSX`/`XP_DARWIN` really means AppKit to `MOZ_WIDGET_COCOA`.
- The build system mostly selects directories by `MOZ_WIDGET_TOOLKIT`, which helps. The real work is in three places:
  - **Link coherence.** Darwin sources are built by OS, but Darwin helpers and frameworks are built or linked by toolkit.
  - **About 20 `#if XP_DARWIN … #elif MOZ_WIDGET_GTK` chains.** Several of these fail silently, for example the key-name tables.
  - **The font plane.** fontconfig in thebes, CoreText in gfx/2d and in WebRender's Rust code.
- Scope: about 120–150 files, about 300–450 patch sites, mostly mechanical. Expect 1–2 weeks to get a first link, then 1–3 weeks of runtime bring-up.

## 1. Configure / build system

### Hard blockers

| Site | Problem |
|---|---|
| `build/moz.configure/pkg.configure:13` | `use_pkg_config` is false for OSX. Every `pkg_check_modules` call is skipped: gtk+-3.0 (also wants gtk+-unix-print-3.0 and gio-unix-2.0), gtk+-wayland-3.0, xkbcommon, fontconfig, freetype2, pango, dbus. **Patch first.** |
| `toolkit/moz.configure:645` | `toolkit_choices` (already known). |
| `toolkit/moz.configure:762` `v4l2()` | Enabled for aarch64+gtk with no kernel check. `toolkit/xre/gfxtest/v4l2test.cpp:12-14` includes `<linux/videodev2.h>`, so compilation fails. Add `target.kernel == "Linux"`. |
| `toolkit/moz.configure:755/758` | VAAPI and Vulkan video are enabled for gtk. They build stubs and dlopen at runtime, so they will probably compile, but gate them to Linux anyway. |
| `toolkit/moz.configure:3587` | `--disable-dbus` exists only `when=use_pkg_config`. Once pkg-config is enabled, pass `--disable-dbus`. |

### OS-keyed settings that really mean Cocoa

- **`toolkit/moz.configure:3979` (`target_is_osx`)** sets `MOZ_MACBUNDLE_NAME`. **`toolkit/moz.configure:3408`** sets `MOZ_CHILD_PROCESS_BUNDLENAME=plugin-container.app`.
- These **toolkit-keyed** settings conflict with the two above:
  - `DIR_MACOS`/`DIR_RESOURCES` at `toolkit/moz.configure:3859`
  - `moz_pkg_format` at `toolkit/moz.configure:3786`
  - the plugin-container.app bundle at `ipc/app/moz.build:94`
  - `packager.mk:108-126` (`Darwin_cocoa`)
- The C++ uses XP_MACOSX for bundle paths:
  - `ipc/glue/GeckoChildProcessHost.cpp:569` (`plugin-container.app/Contents/MacOS/…`)
  - `xpcom/build/BinaryPath.h:95`
  - `browser/app/nsBrowserApp.cpp:488-554`
  - `toolkit/xre/nsXREDirProvider.cpp`
  - `nsAppRunner.cpp:6674`
- **Recommendation:** keep the `.app` layout. Flip those toolkit checks to `target_is_osx` / `OS_ARCH==Darwin`. That needs fewer C++ edits, and nixpkgs' Darwin install path (`stage-package`, `*.app`) keeps working.
- Defaults that are fine but should be disabled for bring-up:
  - `crashreporter_default` (`:3500`)
  - the sandbox OS match (`:1519`; the Mac seatbelt in `security/sandbox/moz.build:24` is selected by OS)
  - the updater
  - widevine (`:1044`)
  - PHC (`memory.configure:90`)
- Recommended flags: `--disable-crashreporter --disable-updater --disable-sandbox --disable-accessibility --disable-dbus --disable-eme --disable-webrtc`.

### How the GTK/Wayland defines come about

- `toolkit` is computed at `toolkit/moz.configure:685`.
- `MOZ_WIDGET_GTK` comes from `toolkit_define` (`:697`).
- `MOZ_WAYLAND` comes from the `wayland_headers` pkg check (`:731-752`).
- `MOZ_X11` is off for `-wayland-only` (`:711`, `:1631`).
- `USE_FC_FREETYPE` and the fontconfig requirement are set by toolkit (`:864`).
- `MOZ_APPLEMEDIA` is set by toolkit (`:912`), so it will be off and there is no VideoToolbox decoding. FFmpeg dlopens `libavcodec.N.dylib`, which works (`FFmpegRuntimeLinker.cpp:39`).
- Nothing else in configure stops OS_ARCH=Darwin + gtk.

### Directory selection

**Selected by toolkit (good):**

- `widget/moz.build:56` (widget/gtk, x11 excluded)
- `gfx/thebes/moz.build:103/125` (gfxPlatformGtk + gfxFcPlatformFontList, not CoreText)
- `gfx/2d/moz.build:64/100/112`
- `gfx/gl/moz.build:9-14` (provider "Linux")
- `gfx/layers/moz.build:247/269/480/536`
- `gfx/cairo/cairo/src/moz.build:16/52`
- `toolkit/xre/moz.build:74/126/214/298/307`
- `accessible/moz.build`
- `dom/system/moz.build:36`
- `uriloader/exthandler/moz.build:27`
- `browser/themes`, `toolkit/themes`
- `ipc/chromium/moz.build:133` (message_pump_glib)

**Selected by OS (Darwin-correct, keep):**

- `ipc/glue/moz.build:102-231` (Mach shmem, semaphores, MessagePump_mac)
- `ipc/chromium/moz.build:81` (Mach channel, kqueue)
- `xpcom/io` (DarwinFileUtils), `xpcom/base:46/238`
- `security/sandbox/mac`
- `toolkit/crashreporter`
- `netwerk/wifi/mac`
- `dom/geolocation/moz.build:46`
- `dom/webserial:36`
- `security/manager/ssl:223` (Keychain)
- `memory/build`, `mozglue`

**Mismatches (link errors):**

- `toolkit/library/moz.build:222`: AppKit, Carbon, CoreAudio, AudioUnit, IOKit, CoreLocation, CoreMIDI, CoreServices, SystemConfiguration, ScreenCaptureKit and cups are linked **only for cocoa**. The OS-built users above need them, so move this list to `OS_ARCH==Darwin`. Linking AppKit is harmless.
- `xpcom/base/moz.build:33,293`: `nsMacUtilsImpl` and `MacHelpers` are built only for cocoa but used by about 14 XP_MACOSX files (sandbox, GeckoChildProcessHost, ContentChild).
- `widget/cocoa/nsCocoaFeatures.mm`: used by about 23 OS-built files (profiler `platform.cpp`, `GraphDriver.cpp`, `ScaledFontMac`, `ProcessUtils_mac.mm`, `dom/system/mac`…). Build it by OS. It only needs Foundation and sysctl.
- `toolkit/library/rust/gkrust-features.mozbuild:24`: `cubeb_coreaudio_rust` requires toolkit==cocoa, while `media/libcubeb/src/moz.build:67-76` defines `USE_AUDIOUNIT_RUST` by OS. The result is missing symbols.
- `gfx/2d/moz.build:64`: MacIOSurface is built by toolkit, but it is referenced under XP_DARWIN in `gfx/layers/ImageContainer.h:69-708`, `gfx/layers/ipc/IOSurfacePort.*`, `gfx/2d/2D.h:2294` and `gfx/webrender_bindings/RenderMacIOSurfaceTextureHost.cpp`.
- `gfx/gl/GLContextProvider.h:39`: declares the CGL provider as the default under XP_MACOSX.

## 2. C++ preprocessor survey

Counts exclude third_party, media/lib*, tests and widget/cocoa.

| Macro | Lines / files |
|---|---|
| XP_MACOSX | 887 / 286 |
| XP_DARWIN | 288 / 112 |
| XP_IOS | 61 / 30 |
| **MOZ_WIDGET_COCOA** | **only 118 / 39** |
| MOZ_WIDGET_GTK | 370 / 125 |
| MOZ_WAYLAND | 249 / 71 |
| MOZ_X11 | 195 / 55 |

Upstream almost always writes XP_MACOSX where it means Cocoa.

### `#if XP_MACOSX/XP_DARWIN … #elif GTK/WAYLAND` chains

A scan found 20. With both macros defined, the Mac branch wins:

| Site | Effect |
|---|---|
| `widget/NativeKeyToDOMKeyName.inc:66→71`, `widget/NativeKeyToDOMCodeName.inc:31→37` | **Silent: GTK keysym tables are never compiled, so every key is "Unidentified".** |
| `gfx/thebes/gfxPlatform.cpp:77,781,920` | Selects gfxPlatformMac, which is not built, so it fails to link. |
| `gfx/webrender_bindings/RenderCompositor.cpp:209` | Software WebRender goes to `RenderCompositorNativeSWGL` (NativeLayerCA). Also `:265`, plus `RenderCompositorNative.cpp:120`, `RenderCompositorLayerNative.cpp:94`. |
| `gfx/2d/Factory.cpp:392,417` | FontType::MAC instead of FONTCONFIG. |
| `gfx/layers/ImageContainer.h:88` | |
| `layout/base/nsLayoutUtils.cpp:9848` | System font. |
| `dom/canvas/DrawTargetWebgl.cpp:6232` | |
| `dom/media/ImagePixelFormat.cpp:11`, `dom/media/webcodecs/VideoDecoder.cpp:30` | |
| `security/manager/ssl/OSKeyStore.cpp:20,64` | Keychain. Fine, keep. |
| `ipc/glue/GeckoChildProcessHost.cpp:295` | MacProcessLauncher. Keep. |
| `StaticPrefList.yaml:10693` | |

### Hot spots by area

- **toolkit/xre (110 lines).** `nsAppRunner.cpp` has about 35 XP_MACOSX blocks, all Cocoa-app lifecycle:
  - Carbon/AppKit includes (`:56`, `:109`, `:200`, `:214`)
  - `InitializeMacApp` (`:2945`, `:3058`, `:5900`, `:6213`, `:6423`)
  - AppleEvents (`:4882`)
  - `ProcessSerialNumber` transform (`:4616`)
  - `DisableAppNap`
  - `MacAutoreleasePool`
  - CoreText font pre-registration (`:6605`)
  - ASWebAuthSession (`:6449`)

  Use `MOZ_WIDGET_COCOA` for these. Also review `nsEmbedFunctions.cpp` (8 blocks) and `nsXREDirProvider.cpp` (28; mostly OS, keep). nsNativeAppSupport is already toolkit-selected (Unix).
- **widget (45).** `WidgetEventImpl.cpp`:
  - `:810` accel fallback is Meta
  - `:1158` drag modifiers
  - `:2094-2302` key→code tables (Insert/Help, Eisu)

  Also `TextEvents.h:261` (Alt ignored for shortcuts), `IMEData.h:451`, `InputData.cpp:692`, `CompositorWidget.h:65/79`, `InProcessCompositorWidget.cpp:19`, `GfxInfoBase.cpp:48/718/1946`, `headless/HeadlessWidget.h:24`. CUPS printing is shared by cocoa and gtk, and is fine.
- **dom/events and layout (~35).**
  - `EventStateManager.cpp:130/2520/3951/4288/4788`
  - `TextComposition.cpp:28`
  - `dom/xul/MenuBarListener.cpp:206/333` (Alt menu access)
  - `nsXULPopupManager.cpp` (7)
  - `nsMenuPopupFrame.cpp:1600`
  - `nsFrameSelection.cpp:2634`
  - `nsSliderFrame.cpp:1097`
  - `nsTreeBodyFrame`

  `ShortcutKeyDefinitions.cpp` already uses MOZ_WIDGET_GTK, which is good.
- **gfx.**
  - thebes: `gfxPlatform.cpp` (14 sites), `gfxFontUtils.cpp:23/407/1316/1356/1531`, `gfxPlatformFontList.cpp:46/273/2233`, `gfxTextRun.cpp:589`, `COLRFonts.cpp:1319`
  - 2d: `Factory.cpp`, `DrawTargetSkia.cpp:43/1074/1952`, `.h:13/212`, `BorrowedContext.h:20`
  - webrender_bindings (28 lines in 9 files): `Moz2DImageRenderer.cpp:24/179/255` (font handles), `WebRenderAPI.cpp:368/385`, `RenderThread.cpp:59/1563/1683-1804`, `RendererOGL.cpp:177`
  - gl: `GLContext.cpp:51/914/1821/2400`, `GLBlitHelper`, `SharedSurface.cpp:31/112` (IOSurface/CGL)
  - layers (49 lines in 23 files): `TextureClient.cpp:57/262/290`, `CompositorBridgeParent.cpp:1626`, `WebRenderMessageUtils.h:178/302`
  - Most of these become `MOZ_WIDGET_COCOA`, or stay XP_DARWIN if you build the Darwin graphics helpers by OS.
- **Fonts.** On GTK, thebes uses gfxFcPlatformFontList (fontconfig + FreeType), while gfx/2d (XP_DARWIN) and WebRender's Rust (`target_os="macos"`) expect CoreText `NativeFontHandle`s. There are two options:
  - **A.** fontconfig everywhere. Patch about 25 C++ sites plus about 8 Rust files.
  - **B.** Keep CoreText. Build CoreTextFontList, gfxMacFont, ScaledFontMac and gfxCoreTextShaper by OS, and override `gfxPlatformGtk::CreatePlatformFontList`, `GetCommonFallbackFonts` and `FontsPrefsChanged` (`gfxPlatformGtk.cpp:406-485`). This needs no Rust changes and gives native rendering. The cost is that GTK chrome uses pango/fontconfig while content uses CoreText.

  I'd start with A, because it matches the GTK UI. B is the fallback if the WebRender font cfg work turns out to be awkward.
- **Must stay Darwin:**
  - `ipc/glue` (SharedMemoryPlatform_mach, CrossProcessSemaphore_mach, GeckoChildProcessHost MacProcessLauncher/posix_spawn, MessagePump_mac, IOThread, NodeController)
  - `ipc/chromium` (Mach channel, kqueue IO pump)
  - `xpcom/base` (nsMemoryReporterManager Mach task info, AvailableMemoryWatcherMac, nsSystemInfo, MacStringHelpers)
  - xpcom/io (nsLocalFileMac/CFURL)
  - mozjemalloc malloc zones
  - `security/sandbox/mac` (seatbelt; disable at first)
  - toolkit/crashreporter (Mach exception server; disable)
  - cubeb-coreaudio, audio_thread_priority (Mach thread policy)
  - Keychain/OSKeyStore, osclientcerts
  - profiler (Mach thread sampling)
  - GMP/RDD process code paths

  This covers about 95% of the xpcom/ipc hits (~229 lines).
- **Event loop note.** Chromium `MessageLoop` `TYPE_UI` on Darwin uses `MessagePumpMac` (CFRunLoop; `ipc/chromium/src/base/message_loop.cc:281`). The Gecko main thread runs through nsAppShell (glib, widget/gtk), so this is probably fine. Do not create an NSApplication: drop `InitializeMacApp`, or it will add a Dock icon and menubar. `chrome_application_mac.mm` is built by `TARGET_OS==OSX`.

## 3. Linux assumptions in widget/gtk and gfx

- widget/gtk is already BSD-portable. It has no XP_LINUX or `__linux__` guards. `DMABufSurface.cpp` guards eventfd with `HAVE_EVENTFD` and uses `HAVE_SYSIOCCOM_H`. DMABuf, gbm and libdrm are dlopened. The headers come from bundled `third_party/drm` and `third_party/gbm` and should compile (the BSD path in drm.h). DMABuf will just be unavailable at runtime.
- **Critical, silent:** `widget/gtk/SHMBufSurface.cpp:171-178` allocates wl_shm through `ipc::shared_memory::Create()` and passes `TakePlatformHandle().get()` as the fd to `wl_shm_create_pool`. On Darwin, `PlatformHandle` is `UniqueMachSendRight` (`ipc/glue/SharedMemoryHandle.h:38`). The mach_port_t converts implicitly to an int fd, so it **compiles but sends a garbage fd**. Replace it with an fd-based allocator: shm_open+unlink, or mkstemp in $TMPDIR followed by ftruncate and mmap. That is about 60 lines, in SHMBufSurface only.
- `/proc`, memfd and epoll do not appear in widget/gtk or gfx. Linux-only dirs are already OS-gated: `netwerk/system/netlink` (`netwerk/system/moz.build:17`), `security/sandbox/linux` (`security/sandbox/moz.build:21`; not built), LSBUtils (`widget/moz.build:305`).
- `toolkit/xre/gfxtest/glxtest.cpp:189` probes `/sys/bus/pci`. It only logs, and the probe fails, so hardware GL gets blocked. That is what you want.
- `gfxFcPlatformFontList`: its sandbox code is under `MOZ_SANDBOX && XP_LINUX`, which is fine. It needs fontconfig configured with macOS font directories. Under seatbelt, content processes would be blocked from /nix/store font paths, which is one more reason to disable the sandbox at first.
- `gfxPlatformGtk.cpp` has its X11 parts under MOZ_X11. Wayland-only is an upstream-supported configuration, so the MOZ_X11-off paths compile on Linux. Fossil: `gfxPlatformGtk.cpp:60` `#if defined(__APPLE__) && defined(Status)`.
- Front-end JS: `AppConstants.platform` checks MOZ_WIDGET_GTK first (`toolkit/modules/AppConstants.sys.mjs:77`) and returns **"linux"**, so about 80 JS `platform == "macosx"` sites take Linux paths, which is good. The **preprocessed XHTML** still uses XP_MACOSX: `browser-sets.inc.xhtml` (19 lines, keysets), `browser-menubar.inc.xhtml` (14), plus about 15 more files. `toolkit/locales/jar.mn:39` `os=Darwin` selects the Mac `platformKeys.properties` (⌘ glyphs), because `xpcom/components/ManifestParser.cpp:258` withholds "likeunix" from Darwin.
- Prefs: `StaticPrefList.yaml` has 65 XP_MACOSX blocks and all.js has 19. Examples: `ui.key.accelKey=224` (`:20259`), `menuAccessKey=0`, `generalAccessKey`, `chromeAccess`, `contentAccess`, `middlemouse.openNewWindow`, `clipboard.autocopy`, `gfx.canvas.accelerated`, `layers.gpu-process.enabled`, `webgl`. These can be overridden in nixpkgs' `defaultPrefsFile` with no source patch. The C++ fallback at `WidgetEventImpl.cpp:810` still needs patching.

## 4. Software WebRender into wl_shm

- **Confirmed path, no EGL:** `RenderCompositorSWGL` → `CompositorWidget::StartRemoteDrawingInRegion` → `WindowSurfaceProvider::CreateWindowSurface` (`widget/gtk/WindowSurfaceProvider.cpp:110-120`, Wayland branch) → `WindowSurfaceWaylandMB` (`WindowSurfaceWaylandMultiBuffer.cpp:297-306`) → `WaylandBuffer::CreateSHM` → `SHMBufSurfaceRGBA` → `wl_shm_create_pool`. SWGL is pure software.
- **What sends it elsewhere:**
  1. `RenderCompositor.cpp:209` XP_DARWIN routes to NativeLayerCA. Patch to `MOZ_WIDGET_COCOA`.
  2. With `gfx.webrender.compositor` on, Wayland uses `RenderCompositorNativeSWGL` + `NativeLayerWayland` (subsurfaces, viewporter; SHM works). Set it to false for bring-up.
  3. Hardware WebRender is chosen if GfxInfo does not block it. `GLLibraryEGL.cpp:468` looks for `libEGL.dylib` and it is absent, so the fallback happens anyway. Force it with `gfx.webrender.software=true`.
  4. Also set `gfx.canvas.accelerated=false`, `webgl.disabled=true` (or leave WebGL to fail), and `layers.gpu-process.enabled=false`. gfxPlatformGtk already disables the GPU process for Wayland. Set `dom.webgpu.enabled=false`, because wgpu on macOS uses Metal plus CoreAnimation surfaces.

## 5. Rust

`cfg(target_os="macos")` sites that mean CoreText/CG, which only matters for font option A:

- `gfx/wr/webrender_api/src/font.rs` (8–55, 239–388; NativeFontHandle)
- `gfx/wr/wr_glyph_rasterizer/src/lib.rs:42-53`, `Cargo.toml:41/48`, `gamma_lut.rs:171-340`
- `gfx/wr/webrender/src/resource_cache.rs:2393/2400`
- `gfx/webrender_bindings/src/lib.rs:32-36`, `moz2d_renderer.rs:30-52,585`, `Cargo.toml:40`

Mechanism: add a custom `--cfg moz_gtk_fonts` through RUSTFLAGS. Cargo honors it in `[target.'cfg(...)']` tables. Then rewrite these as `all(target_os="macos", not(moz_gtk_fonts))`.

Keep as OS: osclientcerts (`toolkit/library/rust/shared/lib.rs:104`), cubeb-coreaudio (fix the gating in gkrust-features.mozbuild:24), audio_thread_priority, crash_helper, glean, neqo/mtu. Harmless: `wr/device/gl.rs:935`, `query.rs:240` (GL only). Defer `gfx/wgpu_bindings` (Metal) by disabling WebGPU.

## 6. Ranked risks and size

| # | Risk | Size |
|---|---|---|
| 1 | **Key-table and chain-order silent failures.** `NativeKeyTo*.inc`, gfxPlatform selection, RenderCompositor. Fix by auditing all 20 chains. | ~20 sites |
| 2 | **wl_shm via Mach shmem.** Silent garbage fd in `SHMBufSurface.cpp:178`. | 1 file, new code |
| 3 | **Link coherence.** Frameworks, nsCocoaFeatures, nsMacUtilsImpl, MacHelpers, MacIOSurface, CGL provider, cubeb_coreaudio_rust. Many iterations of link errors. | ~10 build files + ~30 sites |
| 4 | **Font plane.** thebes, gfx/2d and WebRender Rust must agree. | A: ~25 C++ + ~8 Rust files. B: ~6 methods + moz.build. |
| 5 | **App lifecycle.** nsAppRunner/nsEmbedFunctions Cocoa startup (NSApp, AppleEvents, autorelease pools). | ~45 sites |
| 6 | **Shortcuts.** Prefs (can be overridden), `WidgetEventImpl`/`TextEvents.h` Alt semantics, browser-sets and menubar XHTML, platformKeys locale, ManifestParser likeunix, MenuBarListener. | ~25 files / ~80 lines |
| 7 | **Packaging.** Bundle-layout coherence. | ~6 configure/moz.build sites |
| 8 | **Configure.** pkg-config, toolkit choice, v4l2, dbus. | Trivial but blocks everything else. |
| 9 | **Deferred.** Sandbox, crash reporter, updater, a11y (ATK), WebRTC (`dom/media/systemservices:40` Darwin capture uses AVFoundation, which is OK), geolocation/wifi (CoreLocation/CoreWLAN, OK if frameworks are linked). | |

**Total:** roughly 120–150 files, 300–450 patch sites, about 2–3k diff lines, mostly `s/XP_MACOSX/MOZ_WIDGET_COCOA/`.

**Strategy:** keep OS=OSX and add toolkit guards, following the convention "XP_DARWIN/XP_MACOSX = Darwin OS and system frameworks (Foundation, CoreGraphics, Mach); MOZ_WIDGET_COCOA = AppKit UI". A new OS variant is not worth it: it would touch NSPR, the Rust target, mozglue, IPC and jemalloc, and throw away working Darwin code. You could add a configure helper `target_is_osx_cocoa` for the configure-side checks.

**Prior art.** In the tree there are only fossils: `gfxPlatformGtk.cpp:60` (`__APPLE__`), `packager.mk` `Darwin_cocoa`, and the BSD portability in DMABuf. From memory, not verified (no web search done): MacPorts shipped GTK/X11 Firefox builds in the gtk2 era (Firefox 3.x–~17). Upstream removed non-Cocoa Mac support, so there is no current effort that I know of.

**nixpkgs.** `default.nix:293` hardcodes cairo-cocoa. `:133` turns withWayland off on Darwin. `:373` drops pkg-config on Darwin. `:556-590` drops gtk3, fontconfig, pango, glib, xkbcommon and libdrm. All of these need Darwin overrides. Keep `stage-package` and the `.app` install. Put the pref overrides in `defaultPrefsFile`.
