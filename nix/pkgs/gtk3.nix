# GTK 3 with only the Wayland backend, for Darwin (waybar is a GTK 3 app).
#
# As with GTK 4 (gtk4.nix): upstream forces Wayland and X11 off on Darwin in
# meson.build, and that line is removed here; the Quartz and Broadway backends
# are off, so apps are Wayland clients with no GDK_BACKEND choice to make.
# GL needs EGL, which Darwin lacks until M4: GDK draws with cairo on wl_shm.
#
# GTK 3 needs no key binding patch: its Mac bindings already depend on
# GDK_WINDOWING_QUARTZ, not __APPLE__.
{
  lib,
  gtk3,
  wayland,
  libepoxy,
  linux-input-headers,
}:

(gtk3.override {
  inherit wayland libepoxy;
  waylandSupport = true;
  x11Support = false;
  xineramaSupport = false;
  broadwaySupport = false;
  libGL = null; # no libGL attribute on Darwin; EGL comes through epoxy at runtime
}).overrideAttrs
  (old: {
    pname = "gtk3-wayland";

    patches = (old.patches or [ ]) ++ [
      ../../patches/gtk3/001-no-gdesktopappinfo-on-darwin.patch
    ];

    buildInputs = old.buildInputs ++ [ linux-input-headers ];

    # Khronos' <EGL/eglplatform.h> makes EGLNativeDisplayType an int on Apple;
    # GTK 3 casts a wl_display to it (eglGetDisplay). WL_EGL_PLATFORM selects
    # the Wayland native types. (GTK 4 uses eglGetPlatformDisplay instead.)
    env = old.env // {
      NIX_CFLAGS_COMPILE = old.env.NIX_CFLAGS_COMPILE + " -DWL_EGL_PLATFORM";
    };

    # Later flags win: nixpkgs turns Quartz on for Darwin without X11.
    mesonFlags = old.mesonFlags ++ [
      "-Dquartz_backend=false"
      "-Dwayland_backend=true"
    ];

    postPatch = old.postPatch + ''
      # Let the Wayland backend build on Darwin (upstream disables it there;
      # nixpkgs' postPatch above already dropped the X11 half of the gate).
      python3 - <<'EOF'
      import re
      p = "meson.build"
      s = open(p).read()
      s, n = re.subn(r"if os_darwin\n  wayland_enabled = false\n", "if os_darwin\n", s)
      if n != 1:
          raise SystemExit("gtk3.nix: Darwin backend gate not found in meson.build")
      open(p, "w").write(s)
      EOF
    '';
  })
