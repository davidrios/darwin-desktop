# M2 notes: Hyprland behaviour

## Status (2026-11-21): steps 1 and 2 of M2 done

Checked by hand with VTE terminals and GTK's widget factory: Lua config, keybinds, dwindle tiling, workspaces, focus and move by direction, float, fullscreen, close (step 1); borders with gradients, rounded corners, minimum-size-aware splits, header bars with only a close button (step 2).

Run it: `nix run .#session` (or `COMPOSITOR=…/target/debug/compositor nix run .#session -- <client>` for a development build).

Step 3 (animations) is written and unit-tested, waiting for a check by hand.

Still to do in M2: shadows and blur, layer-shell (bars, launchers), trackpad gestures, groups, special workspaces, window rules, mouse binds.

## Config: Hyprland's Lua config

`~/.config/hypr/hyprland.lua` (or `--config`), Hyprland 0.55+'s Lua API, run in an embedded Lua 5.5 (`mlua`, vendored). When there's no file, a built-in default (`compositor/src/default-config.lua`) applies. SUPER is Command; ALT is left Option.

Implemented (`compositor/src/config.rs`): `hl.config` (general gaps/border/layout, decoration rounding, dwindle options, input follow_mouse, animations enabled), `hl.bind` with the dispatchers below or a Lua function, `hl.exec_cmd`, `hl.dispatch`, `hl.env`, `hl.on("hyprland.start")`, `hl.curve` (bezier and spring), `hl.animation`, `require` next to the config. `hl.get_active_window` / `hl.get_active_workspace` return nil for now. Everything else in `hl` (window rules, monitors, gestures, devices, permissions, layer rules, custom layouts) loads, warns once and does nothing.

Dispatchers: `exec_cmd`, `exit`, `layout("togglesplit")`, `focus({direction})`, `focus({workspace})` (numbers, `+1`/`-1`, `e+1`/`e-1`, `previous`), `window.close`/`kill`, `window.float`, `window.fullscreen`, `window.move({direction})` (swap), `window.move({workspace, follow})`. Others warn when used.

Tests load Hyprland's example config (v0.57.2, `compositor/tests/`) and the project owner's Linux config (`docs/exampleconfig.lua`) without errors. In the latter, `SUPER + SHIFT + q` (close) is shadowed by the fixed Cmd+Shift+Q escape chord.

## Tiling

`compositor/src/dwindle.rs` ports Hyprland's dwindle algorithm (BSD 3-Clause, attributed): split the leaf a new window opens on (the focused window with `use_active_for_splits`, the default), side by side when wider than tall, order by `force_split`, aspect re-evaluated on relayout unless `preserve_split`. `compositor/src/wm.rs`: workspaces (created on use, dropped when left empty), floating windows (dialogs, fixed-size windows, toggled ones) centred above the tiles, one fullscreen window per workspace, focus history, follow_mouse, direction focus by nearest window. Tiles get xdg tiled states. Geometry as Hyprland: work area = output minus `gaps_out`, inner edges minus `gaps_in`, border reserved inside the tile.

A client whose minimum size exceeds its tile is clipped to the tile (Metal scissor), as Hyprland does; popups aren't clipped. Seen with VTE, which won't go below about 300 points wide.

## Terminal: VTE

foot was tried and dropped: it needs C11 threads, `<uchar.h>`, pipe2/accept4 and more on Darwin, too much porting for a test terminal. VTE (GNOME's terminal widget, 0.84.1) built against our Wayland GTK 4 with no source changes except one fix (`patches/vte/001-darwin-fdwalk-stack.patch`: a stack VLA sized from `RLIMIT_NOFILE` overflowed with a high limit and crashed the app on spawning its shell). Its sample app `vte-2.91-gtk4` is the terminal for now (`nix build .#vte`; the default config uses `$TERMINAL`). GNOME Console or Ptyxis (need libadwaita) are the upgrade path.

## Borders and rounding

Metal shaders (`compositor/src/render.rs`). Content is masked to a rounded rectangle with an antialiased signed-distance test (`decoration.rounding`); the border is a second pipeline drawing the ring between the outer rounded rectangle (window plus `border_size`, radius `rounding + border_size`) and the inner one. Colours: `general.col.active_border` / `inactive_border`, in Hyprland's formats (`rgba(RRGGBBAA)`, `rgb(...)`, `0xAARRGGBB`, `{ colors = {...}, angle = deg }`, up to 8 gradient stops), premultiplied. The focused window gets the active colour. Border and rounding follow the visible rectangle (the window cut to its tile when clipped); fullscreen windows get neither.

## Splits and minimum sizes

A new window splits the way dwindle would unless either half would be smaller than the new or the existing window's minimum size (`xdg_toplevel.set_min_size`); then the other direction, else the new window floats.

## Window capabilities, header bars

Only `fullscreen` is advertised in `xdg_toplevel.wm_capabilities`; client fullscreen requests (F11) work like the fullscreen bind. GTK then disables minimize and maximize, but which buttons it shows comes from its `gtk-decoration-layout` setting, which GTK 4 on Wayland reads from the xdg-desktop-portal Settings portal. With no portal (no D-Bus session yet) it falls back to `menu:minimize,maximize,close`. The session launcher (`nix/pkgs/session.nix`) therefore puts a `gtk-4.0/settings.ini` with `gtk-decoration-layout=appmenu:close` on `XDG_CONFIG_DIRS` (a user's own settings.ini still wins). With a portal in M3, the setting comes from there.

## Session launcher

`nix run .#session` runs the compositor with what its clients need: GTK defaults (above), GSettings schemas on `XDG_DATA_DIRS`, the SVG pixbuf loader, GStreamer plugins, VTE as `$TERMINAL` and on `PATH` with the GTK demos. `COMPOSITOR=path` runs a development build.

## Animations

`compositor/src/anim.rs`, following Hyprland's model. An animated value has where it began, where it is and its goal; setting a new goal mid-flight starts again from where it is. Timings come from `hl.animation` leaves in Hyprland's tree (`global` → `windows` → `windowsIn`/`Out`/`Move`, `fade` → `fadeIn`/`Out`, `border`, `workspaces` → `workspacesIn`/`Out`, ...): a leaf not set inherits its parent's, `global` defaults to speed 8 on the built-in `default` bezier (0, 0.75), (0.15, 1). A bezier animation lasts `speed` × 100 ms; a spring (`hl.curve` type spring, `spring = "name"` in the leaf) runs until it settles. Bezier baking and lookup and the spring integration are ported from hyprutils (BSD-3, `compositor/tests/HYPRUTILS-LICENSE`). `animations.enabled = false` or a disabled leaf makes changes instant.

What animates:
- **Opening**: from a window's first buffer, its rectangle from the `windowsIn` style (`popin N%`, `gnomed`, `slide [edge]`) to its place, alpha 0 → 1 (`fadeIn`).
- **Closing**: the window's last textures are kept (`render::snapshot`) and animate to the `windowsOut` style, alpha → 0 (`fadeOut`), drawn above the other windows.
- **Layout changes** (splits, closing neighbours, swaps, float, fullscreen): `windowsMove`.
- **Border colour** on focus change: `border`, mixed in OkLab as Hyprland does; gradients with different stop counts are resampled.
- **Workspace switches**: the workspace left and the one entered get an offset and an alpha from `workspacesOut` / `workspacesIn` styles (`slide`, `slidevert`, `fade`, `slidefade[vert]`, `N%`, a direction), with `general.gaps_workspaces`. Going to a higher number slides left.

How it's drawn: a window's content is laid out at its goal rectangle and mapped into the animated one (moved and scaled), so a window resizing under a tile is stretched for the length of the animation, as in Hyprland. The goal is the client's window geometry where the layout put it, cut to its tile: when a tile grows, the move starts once the client has committed the bigger size. Rounding, border and tile clipping follow the animated rectangle. Input and layout always use the goals. A window that wasn't on screen in the previous frame (a workspace switch) jumps to its place rather than sliding there. A floating window that resizes itself follows without animating.

Not animated yet: moving a window to another workspace without following (it disappears at once), popups, layers (no layer-shell yet), `borderangle`.
