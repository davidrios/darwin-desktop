# M2 notes: Hyprland behaviour

## Status (2026-11-21): steps 1 and 2 of M2 done

Checked by hand with VTE terminals and GTK's widget factory: Lua config, keybinds, dwindle tiling, workspaces, focus and move by direction, float, fullscreen, close (step 1); borders with gradients, rounded corners, minimum-size-aware splits, header bars with only a close button (step 2).

Run it: `nix run .#session` (or `COMPOSITOR=…/target/debug/compositor nix run .#session -- <client>` for a development build).

Step 3 (animations) is written and unit-tested; seen working by hand (2026-11-23).

Layer-shell (bars, launchers), waybar (`docs/waybar-notes.md`) and a Hyprland IPC subset for its modules are written; waybar shows up as a bar (2026-11-23), its Hyprland modules not yet checked in detail.

Shadows and blur (step 4) are written; the shaders compile and every pipeline builds in a unit test (`render::tests`); seen working by hand (2026-11-24).

Mouse binds and the trackpad workspace swipe (step 5) are written and unit-tested (the resize and chord parts); seen working by hand (2026-11-24).

Special workspaces and Lua query objects (step 6) are written and unit-tested (the owner's scratchpad bind runs against them); seen working by hand (2026-11-24).

Window rules (step 7) are written and unit-tested against the owner's rules; seen working by hand (2026-11-24).

Groups (step 8) are written and unit-tested (the owner's group binds run against them, titles render in a test); seen working by hand (2026-11-24). With them every M2 feature in the owner's config exists in some form.

## Config: Hyprland's Lua config

`~/.config/hypr/hyprland.lua` (or `--config`), Hyprland 0.55+'s Lua API, run in an embedded Lua 5.5 (`mlua`, vendored). When there's no file, a built-in default (`compositor/src/default-config.lua`) applies. SUPER is Command; ALT is left Option.

Implemented (`compositor/src/config.rs`): `hl.config` (general gaps/border/layout, decoration rounding, dwindle options, input follow_mouse, animations enabled), `hl.bind` with the dispatchers below or a Lua function, `hl.exec_cmd`, `hl.dispatch`, `hl.env`, `hl.on("hyprland.start")`, `hl.curve` (bezier and spring), `hl.animation`, `require` next to the config. `hl.get_*` queries return window and workspace tables (below). `hl.window_rule` and `hl.gesture` are below. Everything else in `hl` (monitors, devices, permissions, layer rules, workspace rules, custom layouts) loads, warns once and does nothing.

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

## Layer shell

`compositor/src/layers.rs`: `zwlr_layer_shell_v1` through Smithay's `LayerMap` for our one output (a requested `wl_output` is always ours). The map places surfaces by anchor, margin and size and keeps the zone exclusive surfaces leave; tiling's work area is that zone minus `gaps_out`, so a bar pushes the tiles over (animated, `windowsMove`). Layers are arranged again when the output size changes.

Drawing and hit-testing go background, bottom, the workspace (with its transitions and closing windows), top, overlay. A fullscreen window covers the top layer (a bar) but not the overlay, as in Hyprland. Follow-mouse and click-to-focus don't reach a window under a top or overlay surface.

Keyboard: a top or overlay surface with `exclusive` interactivity (a launcher) takes the keyboard while it's mapped and windows can't take it back; `on_demand` surfaces get it when clicked. When a layer surface with the keyboard goes away or stops wanting it, focus returns to the workspace's last focused window. Layer surfaces' popups (menus, tooltips) are tracked, kept on the output and can grab.

Not yet: layer animations (`layersIn`/`Out`, `fadeLayers*`), `hl.layer_rule`, Hyprland's `ignorezero`/blur for layers.

## Hyprland IPC

`compositor/src/ipc.rs`, enough for waybar's `hyprland/*` modules and simple `hyprctl`-style scripts. The compositor exports `HYPRLAND_INSTANCE_SIGNATURE` (`dd_<pid>`: short, because macOS caps unix socket paths at 103 bytes and `$TMPDIR` is long) and serves `$XDG_RUNTIME_DIR/hypr/<signature>/.socket.sock` and `.socket2.sock`; the directory is removed on exit.

Requests (`[flags]/command`, all answered in JSON): `monitors`, `workspaces`, `activeworkspace`, `clients`, `activewindow`, `workspacerules` (empty), `devices` (one keyboard, the active xkb layout), `version`, `systeminfo` (reports `configProvider: lua`, so waybar sends Lua dispatches). `dispatch` takes a Lua expression (`hl.dsp.focus({ workspace = "2" })`, run through the config's own `hl.dispatch`) or the legacy `workspace`, `movetoworkspace[silent]`, `exec`, `killactive`, `togglefloating`, `fullscreen`, `togglesplit`, `exit`.

Events: each frame a snapshot of the workspaces (the active one and any with windows), windows (workspace, app id, title), focus, fullscreen and keyboard layout is compared with the last one, and the differences go out as `createworkspace[v2]`, `workspace[v2]`, `openwindow`, `movewindow[v2]`, `windowtitle[v2]`, `closewindow`, `activewindow[v2]`, `fullscreen`, `activelayout`, `destroyworkspace[v2]`. Window addresses are per-window counters. The one monitor is `darwin-0`, the `wl_output` name waybar matches against.

The session (`nix run .#session -- -- waybar`) puts waybar and `gtk-layer-demo` on `PATH` and ships a waybar config of modules that work here (workspaces, window title, language, disk, load, clock) through `WAYBAR_CONFIG_DIR`, set only when there's no `~/.config/waybar`: waybar ignores `XDG_CONFIG_DIRS` and otherwise falls back to its own Sway example config.

Not yet: special workspaces, monitor events (one output), `hyprctl` itself (not built), `keyword`/`reload`/`binds`/`layers` requests.

## Shadows and blur

`compositor/src/render.rs`, Hyprland's shaders ported to Metal (BSD-3, attributed in the shader source).

**Shadows** (`decoration.shadow`: `enabled`, `range`, `render_power`, `sharp`, `color`, `color_inactive`, `offset`, `scale`; Hyprland's defaults): the box is the window and its border grown by `range`, scaled from its centre and offset; corners fall off with `render_power` from radius `range + rounding + border`, edges linearly to the power; nothing is drawn under the window itself (Hyprland's cut-out). Focused windows use `color`, others `color_inactive` if set. Shadows follow the animated rectangle and alpha; fullscreen windows have none.

**Blur** (`decoration.blur`: `enabled`, `size`, `passes`, `xray`, `noise`, `contrast`, `brightness`, `vibrancy`, `vibrancy_darkness`; Hyprland's defaults): dual Kawase as Hyprland does it. A prepare pass (contrast, brightness), `passes` downsamples each half the size (with vibrancy), as many upsamples, a finish pass (noise, brightness). Our chain uses real half-size textures where Hyprland draws into shrinking regions of full-size buffers; sample offsets are scaled to match (`radius` source pixels down, `radius/2` up). The drawable is readable (`framebufferOnly` off): blurring ends the frame's render pass, runs the passes in the same command buffer and starts a new pass that loads what was there.

What shows it: a window that isn't opaque (not an XRGB buffer and no opaque region over its whole geometry: blur is invisible behind opaque pixels and skipped). Tiled windows show the background (clear colour, background and bottom layers) blurred once per frame, as Hyprland's `new_optimizations`; floating ones blur everything drawn under them at that point, unless `xray`. The blurred backdrop is drawn rounded under the window's content, with its alpha.

Not yet: blur for layers and popups (Hyprland needs layer rules / `blur.popups` for those), `rounding_power` other than 2, blurring only the window's region instead of the whole output (live blur costs a full-output blur per floating translucent window per frame), `ignore_opacity`, `special`, the blur variants (glass, acrylic, ...).

## Mouse binds

`compositor/src/mouse.rs`. Bind keys `mouse:<code>` (Linux button codes: 272 left, 273 right, 274 middle) and `mouse_down`/`mouse_up`/`mouse_left`/`mouse_right` (the wheel) parse into mouse chords (`binds::MouseKey`). A button bind's press and its release stay with the compositor.

- `hl.dsp.window.drag()`: a floating window follows the pointer. A tiled one moves with it (drawn displaced, above the others) and, released over another tile, swaps places with it; released elsewhere it slides back.
- `hl.dsp.window.resize()`: from the corner nearest where the drag started. Floating: the window is configured to the new size (min 50×50), the far corner staying put. Tiled: `Dwindle::resize`, a port of Hyprland's `resizeTarget` with `smart_resizing` (for each axis the nearest split on the dragged side takes the movement, the nearest on the other side is corrected so the opposite edge stays). While resizing tiles every window follows without animating, as Hyprland with `misc:animate_manual_resizes` off.
- Wheel binds fire once per notch of a mouse wheel; trackpad scrolling never triggers them (it would fire dozens of times a swipe; trackpads have gestures).

The cursor is a closed hand while moving and a diagonal resize arrow while resizing.

Trackpad scrolling (`input.rs`): macOS's precise deltas are pixels, while Wayland clients expect libinput's finger units (GTK multiplies finger scrolling by 2.5, `MAGIC_SCROLL_FACTOR`), so they're scaled by 0.2 (tuned by hand: 0.4, from GTK's factor alone, felt twice too fast; both sides are logical units, so the screen scale doesn't enter) and by `input.touchpad.scroll_factor`. macOS's momentum events after the fingers lift are dropped: clients do their own kinetic scrolling from the axis stop, as on Linux; forwarding both made scrolls fly. The direction is macOS's (its natural scrolling setting), not `input.touchpad.natural_scroll`.

## Trackpad gestures

`compositor/src/gesture.rs`. The view takes raw trackpad touches (`setAllowedTouchTypes(Indirect)`, `touches…WithEvent:`: public AppKit, normalized finger positions and the pad's size). `hl.gesture({ fingers, direction, action = "workspace" })` with that many fingers down: after a 4-point dead zone the centroid's movement along the gesture's axis drags the active workspace and its neighbour (one number up or down; past the end only with `workspace_swipe_create_new`), drawn through the workspace transition. Lifting finishes the switch past `workspace_swipe_cancel_ratio` or above `workspace_swipe_min_speed_to_force`, else it slides back; both use the `workspaces` animation. `workspace_swipe_distance` is in Hyprland-like units: a trackpad point is 3 units, so the default 300 is about 3.5 cm. `workspace_swipe_invert` (default on) makes the content follow the fingers.

macOS still runs its own three/four-finger swipes (Spaces, Mission Control) from the same touches: with a three-finger `hl.gesture`, set those to four fingers (or off) in System Settings > Trackpad.

Only the workspace action; other `hl.gesture` actions warn once.

## Special workspaces

`compositor/src/wm.rs`. `special:<name>` workspaces (`special` alone is `special:special`) get negative ids from -98 down, as Hyprland. `hl.dsp.workspace.toggle_special(name)` (and `focus({ workspace = "special:x" })`, IPC `togglespecialworkspace`) shows one over the active workspace or hides it; showing another hides the first. While shown it's the focused workspace: new windows open in it, and focus, move, split and float act in it; its windows are tiled in the same work area and drawn above, the workspace under it dimmed by `decoration.dim_special` (default 0.2). It fades in and out (always a fade, plus the `specialWorkspaceIn`/`Out` style's offset, as Hyprland). Moving a window to it with `follow` shows it; moving its last window out (or closing it) hides it; switching workspace hides it at once. The IPC lists it as `special:<name>` with `activespecial` events and the monitor's `specialWorkspace`.

## Lua queries

`hl.get_active_window`, `get_windows`, `get_window(address)`, `get_active_workspace`, `get_active_special_workspace`, `get_workspaces`, `get_workspace(id or name)`, `get_cursor_pos` return plain tables with Hyprland's field names (`address`, `class`, `title`, `workspace = { id, name, special, ... }`, `floating`, `fullscreen`, `at`, `size`, `pid`, `focus_history_id`, `active`, ...). Lua can't reach the live state, so before a bind or an IPC Lua dispatch runs, the compositor hands the config a snapshot (`config::World`); dispatches queue and run after the function returns, so a function sees the state from before its own dispatches. `window.group` is nil until groups exist.

## Window rules

`compositor/src/rules.rs`, `hl.window_rule({ name, match = { ... }, effects })`, returning a handle with `set_enabled`. A rule applies when all its `match` properties do; strings are regexes matching the whole value (as Hyprland's RE2 `FullMatch`; `negative:` inverts); later rules override earlier ones for the same effect. Matched properties: `class`, `title`, `initial_class`, `initial_title` (the current values: no initial ones kept yet), `float`, `fullscreen`, `focus`, `modal` (has a parent), `workspace` (name as text), `xwayland`/`pin` (always false). A rule with any other property never matches (warned once).

When a window opens (its first commit, title and app id known): `float`/`tile`, `size` and `move` (floating: "x y" expressions with `N%`, `monitor_w/h`, `window_w/h`, `cursor_x/y`, `+ - * /`, parentheses; `move` is applied once the window's size is known), `center` (the default anyway), `workspace` ("3", "special:magic", "... silent" to leave the focus where it is), `fullscreen`, `no_initial_focus`. While it's open (evaluated again whenever the window's class, title, floating, fullscreen, focus or workspace change): `no_focus` (never takes the keyboard), `suppress_event` ("fullscreen" ignores the client's request; "maximize" has nothing to suppress, there's no maximized state), `rounding`, `border_size` (drawn only: the layout still reserves the configured border), `opacity` ("active [inactive]"), `no_shadow`, `no_blur`, `no_anim`, `decorate` (false: no border or shadow). Other effects warn once.

Not yet: workspace selectors in `match` (`w[tv1]`, `f[1]`), `hl.workspace_rule`, `hl.layer_rule`, tags, `initial_*` kept from the first commit, `monitor`, `pin`, `group`, `animation`, `border_color`, `min_size`/`max_size`.

## Groups

`compositor/src/groups.rs`, Hyprland's tabbed groups. A group (`wm::Group`) belongs to a workspace; its current member holds the group's place (a dwindle leaf, swapped with `Dwindle::replace`, or a floating spot) and the others are hidden: kept in the workspace (`Workspace::windows`, so IPC, Lua and commits find them) but not mapped or drawn (`Workspace::shown`). Focusing a hidden member (from focus history, a tab click, the IPC) shows it first.

- `hl.dsp.group.toggle()`: the focused window becomes a group of one; on a grouped window, the group breaks up into tiles next to it.
- `group.auto_group` (default on): a new tiled window opening while the focused window is in an unlocked group joins it as the new current tab, after the current one (`insert_after_current`) or at the end.
- `group.next()`/`prev()` (wrapping), `group.move_window({ forward })` (reorders the tab, wrapping), `group.lock({ action = "lock"|"unlock"|... })` (locked groups take no new windows).
- `window.move({ direction, group_aware = true })` is Hyprland's `movewindoworgroup`: out of a group of more than one into a tile on that side; else into an unlocked group in that direction; else a plain move. IPC: `togglegroup`, `changegroupactive f|b`, `movegroupwindow f|b`, `lockactivegroup`, `movewindoworgroup`.
- Moving a grouped window to another workspace (`window.move({ workspace })`, IPC `movetoworkspace[silent]`, the scratchpad bind into a special workspace) moves the whole group, its tab order and lock with it, as Hyprland; with `follow` the moved window is the tab shown.
- Lua `window.group = { size, current_index (1-based), locked, members = { { address, class, title }, ... } }`; IPC `clients` fill `grouped`.

The tab bar (`group.groupbar`, Hyprland's defaults and layout): reserved above the window inside its tile, `gaps_out × (1 + keep_upper_gap) + indicator_height + indicator_gap + height` tall; a tab per member sharing the width with `gaps_in` between: an indicator line (`col.active` for the focused window, `col.inactive` otherwise, `col.locked_*` when locked; rounded by `rounding`) at the bottom and the title centred in the `height` strip above it (`font_size` points in `font_family`, the system UI font by default; `text_color*`, `text_padding`, `text_offset`). Grouped windows get `group.col.border_*`. Clicking a tab shows that window. Titles are drawn with Core Text (`text.rs`) into BGRA textures, cached by title, size and colour.

Not yet: `stacked` bars, `gradients`, font weights, `round_only_edges` (every tab's indicator is rounded), middle-click close and scrolling on the bar, dragging windows into or out of bars, `group_on_movetoworkspace`, floating groups reserving space (their bar sits above the window).
