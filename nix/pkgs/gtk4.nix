# GTK 4 with only the Wayland backend, for Darwin.
#
# Upstream GTK forces the Wayland and X11 backends off on Darwin in
# meson.build; that one line is removed here. The macOS (Cocoa) backend is
# turned off: apps in this session are Wayland clients, and a single backend
# keeps GDK_BACKEND out of the picture.
#
# GL needs EGL, which Darwin lacks until M4, so GSK renders with cairo.
{
  lib,
  gtk4,
  wayland,
  libepoxy,
  linux-input-headers,
}:

(gtk4.override {
  inherit wayland libepoxy;
  waylandSupport = true;
  x11Support = false;
  vulkanSupport = false;
  libGL = null; # no libGL attribute on Darwin; EGL comes through epoxy at runtime
}).overrideAttrs
  (old: {
    pname = "gtk4-wayland";

    patches = (old.patches or [ ]) ++ [
      ../patches/gtk4/no-gdesktopappinfo-on-darwin.patch
      ../patches/gtk4/color-picker-by-backend.patch
    ];

    buildInputs = old.buildInputs ++ [ linux-input-headers ];

    mesonFlags = old.mesonFlags ++ [
      (lib.mesonBool "macos-backend" false)
      (lib.mesonBool "wayland-backend" true)
    ];

    postPatch = old.postPatch + ''
      # Let the Wayland backend build on Darwin (upstream disables it there).
      python3 - <<'EOF'
      import re
      p = "meson.build"
      s = open(p).read()
      old = "if os_darwin\n  wayland_enabled = false\n  x11_enabled = false\nelse"
      if old not in s:
          raise SystemExit("gtk4.nix: Darwin backend gate not found in meson.build")
      s = s.replace(old, "if os_darwin\n  x11_enabled = false\nelse")
      open(p, "w").write(s)
      EOF
    '';
  })
