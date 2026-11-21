# M1 notes: compositor skeleton (Track 2)

## Status (2026-11-21): exit criterion met

`compositor/` (Rust, MIT OR Apache-2.0) runs the GTK 4 widget factory as a Wayland client in its own window, drawn with Metal, and it's usable: checked by hand (drawing at Retina scale, pointer, scrolling, keyboard and shortcuts, menus and popovers, live resize, full screen, Cmd+Shift+Q). This replaces Cocoa-Way as the test display.

```
nix build                             # the compositor (flake default package): result/bin/compositor
nix develop
scripts/prepare-third-party.sh        # once after cloning, and after changing a queue
cd compositor && cargo run -- $(nix build --no-link --print-out-paths ..#gtk4-demos)/bin/gtk4-widget-factory
```

`compositor [--fullscreen] [-- client args...]` prints `WAYLAND_DISPLAY` and `XDG_RUNTIME_DIR` (it creates `$TMPDIR/darwin-desktop-runtime` when unset) and starts the client with them.

## Design

| Piece | File | How |
|---|---|---|
| Protocols | `state.rs` | Smithay (patched submodule): wl_compositor, wl_subcompositor, wl_shm, xdg_shell with popup grabs, wl_seat, wl_output + xdg-output, wl_data_device, wp_cursor_shape_v1. Handler structure after Smithay's smallvil. |
| Window policy | `state.rs` | Parentless toplevels fill the output (maximized); dialogs keep their size, centred; newest window and clicked window get focus. |
| Event loop | `app.rs` | AppKit owns the main thread. calloop's kqueue fd is a `CFFileDescriptor` on the main run loop; readable means dispatch with zero timeout, then flush clients. Single-threaded; state in a thread-local, with re-entrancy guarded. |
| Frames | `app.rs` | `NSView`'s `CADisplayLink` (display refresh rate). Each tick: follow window size/scale, redraw if anything committed, send frame callbacks, refresh the space. |
| Renderer | `render.rs` | Layer-hosting view on a `CAMetalLayer` (BGRA8). Each surface's shm buffer is uploaded to a texture when its commit counter changes (cached in the surface's data map) and drawn as a quad; premultiplied blending; XRGB forces alpha 1. Subsurfaces and popups placed as Smithay's own renderer does. |
| Keyboard | `input.rs`, `keymap.rs` | A local `NSEvent` monitor takes key down/up and flags changed for our window (AppKit sends no keyUp while Cmd is held; mitsuami does the same). macOS key codes to evdev (table generated from mitsuami-core's tables). Modifiers from the device-dependent flag bits; Caps Lock as press+release; held keys released when the window loses key status. |
| Keyboard layout | `layout.rs` | The xkb keymap is generated from the macOS layout (`UCKeyTranslate`, levels plain/Shift/Option/Option+Shift, dead keys as xkb dead keysyms), following the current input source; left Option is Alt, right Option the level-3 shift. `--keyboard-layout ID`, `--dump-keymap`. |
| Keyboard capture | `capture.rs` | While the window is key, the window server's hot key mode is "all disabled but Universal Access" (`CGSSetGlobalHotKeyOperatingMode`, looked up with `dlsym`), restored when it isn't; presentation options as fallback. `--no-capture` turns it off. Key events log at `RUST_LOG=compositor::input=debug`. |
| Cursor | `cursor.rs`, `app.rs` | cursor-shape protocol shapes map to macOS's own cursors (macOS 15 frame-resize, column/row-resize and zoom cursors where present, as in mitsuami); cursor surfaces become an image `NSCursor` with the client's hotspot at the buffer's scale; hidden is a transparent cursor. Applied each frame when it changes, kept through `resetCursorRects`. GTK 4.24 uses cursor-shape (checked by hand). |
| Pointer | `app.rs`, `input.rs` | View's mouse events in flipped view coordinates (= Wayland logical); buttons to `BTN_*`; precise scrolling as finger axis with stop at gesture end, wheel as v120. |
| Output | `state.rs` | One `wl_output` = the view: logical size in points, integer scale from `backingScaleFactor`, refresh from the screen's `maximumFramesPerSecond`. |

## Known gaps (next)

- **Clipboard:** only between Wayland clients; no bridge to the macOS pasteboard.
- **Input methods:** none yet (fcitx5 over `input-method-v2` in M3, `decisions.md`). Layout switching is through the macOS input menu until there's a keybind and config.
- **Damage:** every frame with a commit re-uploads changed buffers whole and redraws everything.
- **Scale:** integer output scale only; no fractional-scale or viewporter protocols.
- **Smithay logs `surface missing from known popups`** when GTK commits a surface whose `xdg_popup` is already gone (GTK keeps the `wl_surface` when hiding a popover). Harmless so far; check against upstream Smithay.
- **GTK frame-timing warnings** (`gdk_frame_timings_*`), as under Cocoa-Way: no presentation-time feedback yet.
