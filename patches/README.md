# Patch queues

Every change this project makes to someone else's code, one directory per upstream project, applied in filename order. File names carry three digits (`001-…`) so the glob sorts correctly past 9 and 99. Most patches carry a paragraph of what and why above the first diff header, which `git apply` and `patch` ignore.

There are two ways a queue gets applied:

| Queue | Upstream | Applied by |
|---|---|---|
| `smithay/` | `third_party/smithay` submodule, Smithay master `a38bb709` (2026-11-19) | `scripts/prepare-third-party.sh` |
| `reis/` | `third_party/reis` submodule, reis 0.7.1 (`02bc1b5`) | `scripts/prepare-third-party.sh` |
| `wayland/` | libwayland 1.26.0 release tarball | Nix, `nix/pkgs/wayland.nix` |
| `libepoxy/` | libepoxy 1.5.10 (nixpkgs) | Nix, `nix/pkgs/libepoxy.nix` |
| `gtk4/` | GTK 4.24.0 (nixpkgs) | Nix, `nix/pkgs/gtk4.nix` |
| `vte/` | VTE 0.84.1 (nixpkgs) | Nix, `nix/overlay.nix` |

## Submodule queues: `scripts/prepare-third-party.sh`

The Rust crates are built from patched copies. The submodules `third_party/<name>` stay pristine at the commit the superproject pins; prepare, per queue:

1. Checks that the submodule sits at the pinned commit (a queue written for one commit says nothing about another).
2. Exports that commit with `git archive` into `build/third_party/<name>` (gitignored), so stray edits in the submodule can't leak in.
3. Applies the queue in filename order with `patch -p1`, dry-running each patch first. A patch that doesn't apply stops the run and prints why.
4. Writes a stamp (commit + patch hashes). The next run skips a tree whose stamp matches, so Cargo doesn't rebuild the crate for nothing; `-f` forces a rebuild.

`compositor/Cargo.toml` points at `build/third_party/`. The Nix package (`nix/pkgs/compositor.nix`) reads the pristine submodules from the flake source (`inputs.self.submodules = true`) and makes the same copies with the same queues.

**Don't edit `build/third_party/` or the submodules by hand**; change the patch and rerun.

After cloning: `git submodule update --init && scripts/prepare-third-party.sh`.

**Writing or refreshing a patch.** Edit in the submodule (it's a normal git checkout), then write the diff of just your files and put the submodule back:

```
git -C third_party/smithay diff -- src/utils/sealed_file.rs > patches/smithay/00N-what.patch
git -C third_party/smithay checkout .
scripts/prepare-third-party.sh
```

With earlier patches in the queue touching the same file, apply them in the submodule first (`git -C third_party/<name> apply ../../patches/<name>/00[1-M]*.patch`), make your change, and diff against that state (`git -C third_party/<name> stash` the result, or diff two copies). Add the what/why paragraph at the top. The check that proves a patch is prepare applying the whole queue to the pristine export. New files need a `--- /dev/null` header (`git add -N` makes `git diff` produce one).

**Moving the pin.** `git -C third_party/<name> checkout <new>`, run prepare, fix or drop what no longer applies, review the Drop column, commit the new pin together with the queue.

## Nix queues

The C libraries come from nixpkgs and are patched in their derivation's `patches` (`nix/pkgs/*.nix`), applied with `patch -p1` to the release source. Editing a patch rebuilds that package and everything above it on the next `nix build`. Write them as git-format diffs (`a/` and `b/` prefixes) against the release tarball.

## smithay

| Patch | What / why | Drop when |
|---|---|---|
| `001-darwin-sealed-file` | `SealedFile` (keymaps for `wl_keyboard`) on Apple platforms: an unlinked `0600` temporary file behind a read-only fd. The upstream fallback, a POSIX shm object, can't be written with `write(2)` on macOS (`ENXIO`) and can't be mapped `MAP_PRIVATE` (`EINVAL`), which clients must use for keymaps; without this no client gets a keyboard. `docs/smithay-notes.md` | Smithay handles Apple platforms upstream |

## reis

| Patch | What / why | Drop when |
|---|---|---|
| `001-darwin-socket-flags` | Build on Darwin: no `MSG_NOSIGNAL` (set `SO_NOSIGPIPE` on the socket instead) and no `MSG_CMSG_CLOEXEC` (set `FD_CLOEXEC` on received fds). Smithay master depends on reis unconditionally | reis builds on Darwin upstream, or Smithay makes it optional |

## wayland

| Patch | What / why | Drop when |
|---|---|---|
| `001-darwin` | The Darwin series from `barracuda156/wayland-mac` (branch `1.26.0-mac`, commit `eb50623f`), as MacPorts ships it: owl-compositor's fixes (peer credentials, CLOEXEC fallbacks, epoll-shim, shm without memfd/`posix_fallocate`/`mremap`, `TMPDIR` fallback). Regenerate from `https://github.com/barracuda156/wayland-mac/compare/87cc8a87...<rev>.diff`. The nix package also unregisters one test, `send_overflow_disconnection` (`docs/m0-notes.md`) | libwayland supports Darwin upstream |

## libepoxy

| Patch | What / why | Drop when |
|---|---|---|
| `001-egl-on-darwin` | Upstream commit 47d975d (2025-10-29), `src/` hunks only: EGL dispatch on Apple, loading `libEGL.dylib`. GTK's Wayland backend includes `<epoxy/egl.h>` | libepoxy releases past 1.5.10 |

## gtk4

| Patch | What / why | Drop when |
|---|---|---|
| `001-no-gdesktopappinfo-on-darwin` | Guard `GDesktopAppInfo` in the Wayland launch context; GLib on Darwin has none (only used for a Flatpak app-id lookup) | GLib on Darwin provides desktop app info (also an M3 need) |
| `002-color-picker-by-backend` | Choose the Quartz color picker by `GDK_WINDOWING_MACOS`, not `__APPLE__`; otherwise GTK fails to link without the macOS backend | Upstream makes the same change |
| `003-mac-bindings-by-backend` | Mac-style key bindings (Meta+C/Z/A…, Ctrl+click as secondary click) only with the macOS backend, so apps use Linux bindings (`decisions.md`, 2026-11-20). 31 blocks in 13 files | Upstream ties these to the backend |

The nix package also removes upstream's "no Wayland on Darwin" line from GTK's `meson.build` (`nix/pkgs/gtk4.nix`, a one-line substitution rather than a patch).

## vte

| Patch | What / why | Drop when |
|---|---|---|
| `001-darwin-fdwalk-stack` | Darwin `fdwalk()` sized its `proc_pidinfo(PROC_PIDLISTFDS)` buffer, a stack VLA, from `RLIMIT_NOFILE`; with a high limit (1048576, common in shells) that's 8 MiB and the app crashed spawning its shell. Now sized from the fds actually open (a NULL-buffer call returns the size), still without malloc, so still safe after fork | Upstream fixes it |
