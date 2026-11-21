# M2 notes: Hyprland behaviour

## Status (2026-11-21): step 1 of M2 done

Checked by hand with VTE terminals: Lua config, keybinds, dwindle tiling, workspaces, focus and move by direction, float, fullscreen, close.

Still to do in M2: borders and rounding (Metal shaders), animations (curves and springs from the config), shadows and blur, layer-shell (bars, launchers), trackpad gestures, groups, special workspaces, window rules, mouse binds, don't split below a window's minimum size.

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
