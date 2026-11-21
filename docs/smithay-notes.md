# Smithay on macOS

Outline section 7: "does Smithay build and run on current macOS without patches?" Checked 2026-11-21 against Smithay master `a38bb709` (2026-11-19; latest release 0.7.0 is from 2025-06), `default-features = false, features = ["wayland_frontend"]`, Rust 1.99.1 from nixpkgs.

## Result: two small patches needed

1. **It doesn't build.** Master depends on `reis` (libei) unconditionally, and reis 0.7.1 uses Linux-only `MSG_NOSIGNAL` and `MSG_CMSG_CLOEXEC`. Fix: `patches/reis/001-darwin-socket-flags.patch`. On Apple, send without the flag and set `SO_NOSIGPIPE` on the socket; receive without the flag and set `FD_CLOEXEC` on each received fd.
2. **Keymaps can't be shared.** `SealedFile` (used for `wl_keyboard.keymap`) falls back to `shm_open` + `write(2)` off Linux/FreeBSD. On macOS, writing to a POSIX shm object fails with `ENXIO`, and such objects can't be mapped `MAP_PRIVATE` either (`EINVAL`), which clients must use for keymaps (`wl_keyboard` v7+). Without a fix, no client gets a keyboard. Fix: `patches/smithay/001-darwin-sealed-file.patch`. On Apple, write a `0600` temporary file, reopen it read-only, unlink it, and hand out the read-only fd.

Verified with a probe program: 64 KiB round-trips through a `MAP_PRIVATE` read-only mapping, a writable shared mapping of the fd is refused, and no temporary file is left behind.

With both patches, `smithay` with `wayland_frontend` compiles on aarch64-darwin. It links against libxkbcommon (nixpkgs' works).

Both are written for this project (Cocoa-Way carries its own different fix for the second; nothing was copied) and are suitable for upstream PRs.

## Notes from reading Cocoa-Way's setup (facts only)

- Pins an older Smithay master commit (`6329f5b`, before reis became a hard dependency) with eight upstream backports.
- Patches `wayland-backend` for fair, budgeted client dispatch. That's a product choice, not a Darwin requirement; worth remembering if one client can flood the compositor.
- Uses winit for the window and input on macOS, and its own Metal renderer for SHM buffers.

## How the patches are carried (decided 2026-11-21)

As patch queues, the way 2ksbox carries QEMU: Smithay and reis are pinned submodules in `third_party/`, their queues are `patches/smithay/` and `patches/reis/`, and `scripts/prepare-third-party.sh` applies them. See `patches/README.md`. Upstream PRs can still follow; a merged one drops its patch.
