# Firefox port: brief (2026-11-24)

The brief for the agent porting Firefox. It is M3 step 0 and step 6 in `m3-plan.md`. Read this first, then:
- `docs/firefox-notes.md`: the source survey, with every file and line referred to below.
- `docs/HANDOFF.md`: the project, how to build, and how to work with the owner.
- `docs/decisions.md`: why things are the way they are.
- `docs/gtk4-notes.md`: the GTK port, the closest precedent for this work.

## Goal

Firefox 160 (the version nixpkgs packages, `firefox-unwrapped`) builds on aarch64-darwin with the GTK 3 Wayland-only toolkit and runs as a Wayland client of this project's compositor. It's the owner's daily browser, and **a Mac browser beside the session doesn't count** (owner, 2026-11-24): M3 can't exit without it.

**Exit:** the owner uses it for a working day in the session. That means pages, tabs, typing with Linux shortcuts (Ctrl+C/V/T/W/L), video with sound, downloads and the file chooser, extensions and Sync.

## Decided

- **The OS stays OSX; guards follow the toolkit.** Convention for every patch:
  - `XP_DARWIN`/`XP_MACOSX` mean the Darwin OS and the system frameworks that aren't AppKit: Mach IPC and shared memory, Foundation, CoreGraphics, CoreText, CoreAudio, Keychain, jemalloc zones.
  - `MOZ_WIDGET_COCOA` means AppKit UI.
  - Change only the sites where "macOS" really means AppKit. A new OS variant is rejected: it would ripple through NSPR, the Rust target, mozglue, IPC and jemalloc, and throw away working Darwin code.
- **Fonts: CoreText** (owner, 2026-11-24; option B in `firefox-notes.md` section 2). Page content keeps the Mac font path: gfx/2d, `ScaledFontMac` and WebRender's Rust `NativeFontHandle` stay as they are on macOS.
  - Build `CoreTextFontList`, `gfxMacFont`, `gfxCoreTextShaper` and `ScaledFontMac` by OS rather than by toolkit.
  - In `gfxPlatformGtk` (`gfx/thebes/gfxPlatformGtk.cpp:406-485`), under `XP_DARWIN`, return the CoreText font list from `CreatePlatformFontList`, the Mac fallback list from `GetCommonFallbackFonts`, and the matching font type in `FontsPrefsChanged`.
  - Keep `gfx/2d/Factory.cpp:392,417` on `FontType::MAC`.
  - No Rust font changes. GTK's own UI (pango, fontconfig) uses fontconfig. That's accepted, but fontconfig must see the macOS font directories (`/System/Library/Fonts`, `/Library/Fonts`, `~/Library/Fonts`).
  - Why: about 6 methods and a few moz.build lines instead of about 25 C++ sites and 8 Rust files, each carried release after release.
- **Rendering:** software WebRender (SWGL) presenting through wl_shm, with no EGL until M4.
  - Bring-up prefs (in nixpkgs' `defaultPrefsFile` or the package's prefs, not source patches): `gfx.webrender.software=true`, `gfx.webrender.compositor=false`, `gfx.canvas.accelerated=false`, `layers.gpu-process.enabled=false`, `dom.webgpu.enabled=false`, and WebGL off or left to fail.
- **wl_shm memory:** `widget/gtk/SHMBufSurface.cpp:171-178` hands a Mach send right to `wl_shm_create_pool` as if it were a file descriptor. Replace it with an fd-based allocator under `XP_DARWIN`: `shm_open` and `shm_unlink`, or `mkstemp` in `$TMPDIR` and unlink, then `ftruncate` and `mmap MAP_SHARED`.
  - Darwin shm objects can't be `write(2)`n or mapped `MAP_PRIVATE` (Smithay's `SealedFile` patch, `patches/smithay/`).
- **No NSApplication.** Firefox must not create one, or it gets a Dock icon and menu bar and fights the compositor. The `InitializeMacApp`, AppleEvents, process-transform and similar blocks in `toolkit/xre/nsAppRunner.cpp` and `nsEmbedFunctions.cpp` become `MOZ_WIDGET_COCOA`.
- **Linux shortcuts** (`decisions.md`, 2026-11-20): Ctrl is the accelerator and Alt opens menus; Cmd belongs to the compositor and reaches clients as Super.
  - Prefs: `ui.key.accelKey=17`, `ui.key.menuAccessKey=18`, and the rest of the Linux values for the `StaticPrefList.yaml` XP_MACOSX blocks.
  - Source: `WidgetEventImpl.cpp:810`, `TextEvents.h:261`, `MenuBarListener.cpp`, `browser-sets.inc.xhtml`, `browser-menubar.inc.xhtml`, `toolkit/locales/jar.mn:39`, and `ManifestParser.cpp:258` ("likeunix" for Darwin + GTK).
  - The front-end JS already reports `AppConstants.platform == "linux"`.
- **Bring-up configure flags:** `--enable-default-toolkit=cairo-gtk3-wayland-only --disable-crashreporter --disable-updater --disable-sandbox --disable-accessibility --disable-dbus --disable-eme --disable-webrtc`.
  - Turn D-Bus back on once the session has a bus (M3 step 3). Revisit WebRTC (camera and microphone go through AVFoundation) and the seatbelt sandbox after it runs.
- **Packaging:** keep the `.app` bundle layout, so nixpkgs' Darwin `stage-package` install keeps working. Flip the toolkit-keyed bundle checks (`firefox-notes.md` section 1) to OS-keyed.
- **Profile:** separate from any Mac Firefox profile. Decide where (e.g. `~/.mozilla`, as on Linux) and record it.

## Work in stages

Each stage ends with something checkable. Record findings in `docs/firefox-notes.md` (add a "Port log" section) as you go.

1. **Configure.**
   - Turn pkg-config on for OSX when the toolkit is GTK (`build/moz.configure/pkg.configure:13`).
   - Allow the GTK toolkits for OSX (`toolkit/moz.configure:645`).
   - Gate V4L2, VAAPI and Vulkan video to Linux (`:755-762`).
   - Check: configure passes with `MOZ_WIDGET_GTK`, `MOZ_WAYLAND`, no `MOZ_X11`.
2. **Compile.**
   - Audit all 20 `#if XP_MACOSX/XP_DARWIN … #elif MOZ_WIDGET_GTK` chains (`firefox-notes.md` section 2). Some fail silently, such as the GTK key tables in `widget/NativeKeyTo*.inc`, so every chain must be read, not only fixed when the compiler complains.
3. **Link.**
   - Link the frameworks for Darwin, not only for Cocoa (`toolkit/library/moz.build:222`).
   - Build by OS: `nsMacUtilsImpl`/`MacHelpers`, `nsCocoaFeatures`, MacIOSurface (or guard its users), the CoreText font sources.
   - Make `cubeb_coreaudio_rust` follow the OS (`gkrust-features.mozbuild:24`).
   - Make the CGL provider default `MOZ_WIDGET_COCOA` (`GLContextProvider.h:39`).
   - Expect many rounds.
4. **Start.** `firefox --version` and a headless run (`--headless --screenshot`), which tests startup without the widget layer.
5. **First window.** Under the compositor (`COMPOSITOR=… nix run .#session -- -- firefox`): a window with drawn chrome, through SWGL and wl_shm. This is where the SHMBufSurface allocator and `RenderCompositor.cpp:209` matter.
6. **Pages and input:** real sites, scrolling, text input, popups and menus, Linux shortcuts, clipboard.
7. **Media and the rest:**
   - Video and sound: FFmpeg is dlopened (`libavcodec.N.dylib`); provide it on the library path. Audio goes through cubeb, CoreAudio or PulseAudio.
   - Downloads, file chooser, extensions, Sync, fractional scale (M3 step 4).
8. **Package** into the session: `.desktop` file, on `PATH` in `nix/pkgs/session.nix`.

## How to work

**Iterate with mach, ship with Nix.** A Nix build from scratch takes over an hour, so it's no way to iterate.
- Unpack the source nixpkgs uses (`firefox-unwrapped.src`, a `firefox-160.0.1.source.tar.xz`) into `build/firefox/` (gitignored) as a git repository, with the pristine tree as its first commit.
- Work in commits there and build incrementally with `./mach build`, inside a shell with the package's build inputs:
  - `nix develop .#firefox` once the scope has a `firefox` attribute (a `nix develop` on a package gives its build environment);
  - or a dedicated dev shell in `flake.nix`.
- Keep a `mozconfig` there that mirrors the derivation's configure flags.

**Patch queue.** Export the commits with `git format-patch` into `patches/firefox/NNN-*.patch`, each with a paragraph of what and why, and document the queue in `patches/README.md` (what, why, when it can be dropped). Grouping by area keeps the queue reviewable across Firefox releases: configure, link, key handling, fonts, wl_shm, startup, shortcuts.

**Nix package.** Add `nix/pkgs/firefox.nix` to the `darwinWayland` scope (`nix/overlay.nix`). Never override top-level `firefox-unwrapped` (`decisions.md`, 2026-11-20).
- Override nixpkgs' `buildMozillaMach` result for Darwin: toolkit (`default.nix:293`), `withWayland` (`:133`), pkg-config (`:373`), and the GTK, fontconfig, pango, glib and xkbcommon inputs (`:556-590`), all taken from the scope (its `gtk3` and `wayland`).
- Apply the queue and set the prefs there. Then wrap it the way the session expects (`gtk4-demos.nix` and `session.nix` show the runtime environment GTK needs).

**Testing.** You can't see the screen (no Screen Recording permission): use logs, `--headless --screenshot` runs (you can read the PNG), and `MOZ_LOG`. The owner runs it under the compositor and reports. Ask before assuming something works.

**Long builds** go in the background.

**Nix on this machine:**
- Put `/nix/var/nix/profiles/default/bin` on `PATH`.
- Pass `--extra-experimental-features 'nix-command flakes'` to every `nix` command.
- The flake only sees files git knows: `git add` new files before `nix build`.

## Sharing the repository

Another agent works on the compositor in this repository at the same time (`compositor/`, `docs/m3-notes.md`). To stay out of each other's way:
- Keep to `patches/firefox/`, `nix/pkgs/firefox*.nix`, `docs/firefox-*.md`, and small additions to `nix/overlay.nix`, `flake.nix`, `patches/README.md` and `docs/decisions.md`.
- Work in a git worktree on a branch of your own if you'll have uncommitted changes for long.
- Don't change `compositor/`. If Firefox needs something from the compositor (a protocol, a bug fix), write it down in `docs/firefox-notes.md` and tell the owner.

## Owner's rules (from `~/.claude/CLAUDE.md` and the handoff)

- **Commits:**
  - Commit or push only when asked. "Commit and push" after each verified step is the usual pattern.
  - Commits are signed by the owner's git config (SSH); don't disable signing.
  - End commit messages with only the `Co-Authored-By` line. **Never put the Claude session link in a commit message.**
- **Tags:** always signed.
- **Dependencies:** pinned exact versions for anything distributed as a binary.
- **Decisions** go in `docs/decisions.md` (newest first), findings in the notes, with honest "not done yet" lists. Mark steps the owner hasn't checked as "not yet checked by hand".
- **Licence:** Firefox patches are under the MPL 2.0, as upstream; this project's own code is MIT OR Apache-2.0.

## Known risks

In order (details in `firefox-notes.md` section 6):
1. Silent failures from chain order.
2. The wl_shm handle.
3. Link coherence.
4. The font plane (smaller with CoreText).
5. Cocoa startup code.
6. Shortcuts.
7. Packaging.

**If the port stalls**, tell the owner before switching course. WebKitGTK (GTK 4 API, CPU rendering with Skia) is the fallback for a browser in the session, and choosing it is their decision.
