# Handoff for a new session (2026-11-24)

Read this first, then `docs/decisions.md` (why things are the way they are) and the milestone notes it points to. `docs/PROJECT_OUTLINE.md` is the original plan; its sections 2 and 7 have dated updates.

## What this is

A Hyprland-style Wayland desktop on Apple Silicon macOS, keeping Apple's kernel and drivers. **Track 2** was chosen: our own compositor in Rust on **Smithay**, rendering with **Metal**, running (Stage A) as an ordinary AppKit app. Nix flake for everything; pinned exact dependencies (binary project). Licence MIT OR Apache-2.0. Repo: `github.com:davidrios/darwin-desktop`, branch `main`, pushed up to `53f3ff7`.

## Where things stand

| Milestone | State | Notes |
|---|---|---|
| M0 userland | done | patched libwayland, server/client spike (`docs/m0-notes.md`) |
| GTK go/no-go | go | GTK 4.24, Wayland backend only, Linux key bindings (`docs/gtk4-notes.md`) |
| M1 compositor skeleton | done | AppKit window, Metal, input, cursors, keyboard capture, generated keymaps (`docs/m1-notes.md`) |
| M2 Hyprland behaviour | done (steps 1-8) | every feature the owner's config uses, in some form (`docs/m2-notes.md`) |
| M3 a session you can live in | not started | see "Next" below |

M2 steps, all checked by hand by the project owner on the M1 MacBook (the assistant can't see the screen: no Screen Recording permission; it reads logs and runs unit tests):

1. Lua config, keybinds, dwindle tiling, workspaces.
2. Borders, rounding, min-size splits, session launcher.
3. Animations from `hl.curve`/`hl.animation` (`anim.rs`).
4. Shadows and dual-Kawase blur (`render.rs`).
5. Mouse binds (drag/resize), trackpad workspace swipe (`mouse.rs`, `gesture.rs`), trackpad scroll scaling.
6. Special workspaces, Lua query objects (`hl.get_active_window()` etc.).
7. Window rules (`rules.rs`).
8. Groups with a tab bar, titles drawn with Core Text (`groups.rs`, `text.rs`).

Alongside: wlr-layer-shell (`layers.rs`), waybar 0.16 built on our Wayland GTK 3 (`docs/waybar-notes.md`), and a Hyprland IPC subset for waybar's `hyprland/*` modules (`ipc.rs`). Each section of `docs/m2-notes.md` ends with what's not done yet.

## Next

M2 is closed as far as the owner's config goes; leftovers (stacked groupbars, layer rules, workspace selectors in rules, `hl.workspace_rule`, layer animations, blur limited to a window's region) are listed in `docs/m2-notes.md`. M3 (`docs/PROJECT_OUTLINE.md` section 4) hasn't been planned yet: Qt 6, an editor and a browser; kiosk presentation; HiDPI and 120 Hz pacing; clipboard (exists between Wayland clients, not with macOS); audio; launch at login; a nix-darwin configuration. Propose a plan before starting.

## Build and run

Flakes aren't enabled in `/etc/nix/nix.conf` on this machine: put `/nix/var/nix/profiles/default/bin` on `PATH` and pass `--extra-experimental-features 'nix-command flakes'` to every `nix` command (shortened to `nix` below).

```
nix develop                                   # REQUIRED for cargo: libxkbcommon etc. come from here
git submodule update --init && scripts/prepare-third-party.sh   # once, and after changing a queue
cd compositor && cargo build && cargo test
COMPOSITOR=$PWD/target/debug/compositor nix run ..#session -- -- waybar   # a dev run (from compositor/)
nix run .#session                              # packaged build (from the repo root)
nix flake check                                # spike test + compositor build and unit tests
```

- A plain `cargo build` outside `nix develop` fails to link (`library 'xkbcommon' not found`).
- `nix run` eats the first `--`; the compositor needs its own `--` before a client command, hence `-- -- waybar`.
- The flake only sees files git knows: `git add` new files before `nix build` (staging is enough).
- The owner's real config is in `~/.config/hypr/hyprland.lua` (a copy of `docs/exampleconfig.lua`); it starts waybar itself (`hl.exec_cmd("waybar")`), so then no client argument is needed. Without that file the built-in `compositor/src/default-config.lua` applies (float is Cmd+V there).

`nix run .#session` sets the environment clients need (`nix/pkgs/session.nix`): GTK 3 and 4 `settings.ini` defaults (only a close button), GSettings schemas, icon themes, SVG pixbuf loader, GStreamer, VTE as `$TERMINAL`, waybar and `gtk-layer-demo` on `PATH`, and `WAYBAR_CONFIG_DIR` pointing at a working default bar config when there's no `~/.config/waybar`. Compositor flags: `--config hyprland.lua`, `--fullscreen`, `--no-capture`, `--keyboard-layout <macOS id>`, `--dump-keymap`, `-- <client>`. Useful logging: `RUST_LOG=info,compositor::wm=debug,compositor::input=debug,compositor::ipc=debug,smithay=warn`. **Cmd+Shift+Q leaves** (hard-wired, not rebindable).

Running it opens a window on the owner's screen: they test and report. Long builds (GTK ~10 min, VTE ~5 min, waybar's stack much longer) are best run in the background.

## Layout

```
compositor/            Rust crate (Smithay via build/third_party, objc2 AppKit/Metal/CoreText, mlua, regex)
  src/app.rs           AppKit window/view, run loop integration, display link, key monitor, touches
  src/state.rs         Smithay protocol handlers
  src/wm.rs            workspaces (incl. special), tiling policy, focus, actions, group model
  src/dwindle.rs       Hyprland's dwindle algorithm and smart resizing (BSD-3, attributed)
  src/anim.rs          animations: curves, springs, leaf tree, window/workspace transitions
  src/render.rs        Metal renderer: textures, rounding, borders, shadows, blur, group bars
  src/config.rs        Hyprland Lua config (hl API), settings, Lua query objects
  src/rules.rs         window rules       src/groups.rs   groups and the tab bar
  src/layers.rs        layer shell        src/ipc.rs      Hyprland IPC sockets
  src/mouse.rs         drag/resize binds  src/gesture.rs  trackpad workspace swipe
  src/text.rs          Core Text title rendering
  src/binds.rs input.rs keymap.rs layout.rs capture.rs cursor.rs
  src/default-config.lua
  tests/               Hyprland's example config, Hyprland and hyprutils licences
nix/overlay.nix        darwinWayland scope: wayland, libepoxy, gtk3, gtk4, gtkmm3, gtk-layer-shell, vte, waybar, compositor, session
nix/pkgs/*.nix
patches/<project>/NNN-*.patch + patches/README.md (what, why, drop when)
third_party/smithay, third_party/reis   pristine pinned submodules
build/third_party/     patched copies made by scripts/prepare-third-party.sh (gitignored)
spikes/m0-wayland/     M0 server/client spike
docs/                  plan, decisions, milestone notes, waybar notes
```

## Decisions to know (details in docs/decisions.md)

- **Track 2 on Smithay**, no Cocoa-Way fork (it's GPL; only a reference). Smithay master `a38bb709` plus patches.
- **Patch queues** like 2ksbox's QEMU: `patches/<name>/`, pristine submodules, prepare script exports and patches into `build/`; Nix applies the same queues (`inputs.self.submodules = true`). Never edit `build/third_party` by hand.
- **Config is Hyprland's Lua config** (`~/.config/hypr/hyprland.lua`); unsupported `hl.*` calls warn once and no-op. SUPER = Cmd. Behaviour follows Hyprland v0.57.2's source wherever it's ported (with attribution).
- **Apps use Linux shortcuts** (Ctrl+C); Cmd belongs to the compositor. GTK patched for this.
- **Keyboard layouts are generated from the macOS layout** (`UCKeyTranslate`), follow the input source; left Option = Alt, right Option = level-3 (Mac characters).
- **No macOS input-method bridge** (would break without WindowServer in M5); fcitx5 later. Own settings app later.
- **Keyboard capture** via `CGSSetGlobalHotKeyOperatingMode` (as 2ksbox/mitsuami); the window server restores it on exit, even after `kill -9`.
- **Terminal is VTE's sample app** (`vte-2.91-gtk4`); foot was tried and dropped as too much porting.
- **Waybar on GTK 3**: waybar 0.16 still needs gtkmm3 and the GTK 3 gtk-layer-shell, so a Wayland-only GTK 3 lives in the scope too.

## Gotchas learned the hard way

- GTK on Wayland reads settings only from the xdg-desktop-portal Settings portal; without D-Bus it uses built-in defaults. Hence the session's `settings.ini` files.
- Waybar ignores `XDG_CONFIG_DIRS`; only `WAYBAR_CONFIG_DIR`, `~/.config/waybar`, `/etc/xdg/waybar` and its install prefix.
- Darwin shm objects can't be `write(2)`n or mapped `MAP_PRIVATE`: Smithay's keymap `SealedFile` patch.
- macOS caps unix socket paths at 103 bytes and `$TMPDIR` is long: the Hyprland IPC signature is short (`dd_<pid>`).
- Metal shaders compile at runtime: `render::tests` compiles them and builds every pipeline so errors show in `cargo test`, not on screen.
- Trackpad scroll: macOS deltas are pixels and GTK multiplies finger scrolling by 2.5; deltas are scaled by 0.2 (tuned by hand) and macOS momentum is dropped (clients do kinetic scrolling).
- macOS's own three/four-finger swipes still fire alongside the compositor's gestures: with a three-finger `hl.gesture`, macOS's swipe between full-screen apps should be on four fingers or off (System Settings > Trackpad).
- Lua can't reach live state: `hl.get_*` read a snapshot handed to the config before each bind; dispatches run after the function returns.
- AppKit sends no keyUp while Cmd is held: keys come through a local `NSEvent` monitor.
- `/usr/bin/env` strips `DYLD_*` (SIP). XQuartz's old epoll-shim in `/opt/X11` breaks manual builds. MacPorts here is broken (needs `port migrate`).
- `nixpkgs` marks `wayland` badPlatforms on Darwin on purpose: everything patched lives in the `darwinWayland` scope, never overriding top-level.
- The Nix compositor package copies only listed files (`nix/pkgs/compositor.nix` fileset): add new test inputs there.
- VTE crashed on a high `RLIMIT_NOFILE` (stack VLA in `fdwalk`): `patches/vte/001`.
- Known harmless noise: Smithay `surface missing from known popups`, GTK `gdk_frame_timings_*` warnings, "Unable to acquire session bus", waybar's portal / machine-id warnings.

## Working with the project owner

- Global rules (`~/.claude/CLAUDE.md`): **never put the Claude session link in commit messages**; tags signed; pinned exact deps for binaries, caret deps for shared libraries. Commits are signed by their git config (SSH); don't disable signing.
- Commit or push only when asked; "commit and push" after each verified step is the pattern. End commit messages with the `Co-Authored-By` line only. Steps they haven't checked yet can be committed when asked, marked "not yet checked by hand" in the notes.
- They verify visually and report; ask before assuming something works. They like short status, decisions recorded in `docs/decisions.md`, findings in milestone notes, and honest "not done yet" lists.
- Prior art worth reading before designing macOS-side pieces: **mitsuami** (`~/work/rust-ui-toolkit`, their Rust UI toolkit, MIT OR Apache-2.0: AppKit surface, keys, cursors, hot key mode) and **2ksbox** (`~/work/win-98-xp-virt`: patch queues, keyboard capture docs).
- Hyprland v0.57.2 source for reference: clone sparse into a scratch directory (`src/layout`, `src/config`, `src/animation`, `src/desktop`, `src/render`, `example`) plus hyprutils for the animation maths; not kept in the repo.
