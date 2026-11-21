# Darwin userland for the Wayland desktop.
#
# Everything lives in the `darwinWayland` scope instead of overriding top-level
# attributes. nixpkgs marks `wayland` as a bad platform on Darwin on purpose:
# enabling it globally flips Wayland support on in packages such as qtbase and
# breaks them. Inside this scope, `callPackage` resolves `wayland` to the
# patched build, so packages added here link against it without touching the
# rest of nixpkgs (and the public binary cache stays usable for everything else).
final: prev: {
  darwinWayland = final.lib.makeScope final.newScope (self: {
    wayland = self.callPackage ./pkgs/wayland.nix { inherit (prev) wayland; };

    # Unpatched nixpkgs builds already work on Darwin; re-exported for one place to look.
    inherit (prev) wayland-scanner wayland-protocols;

    linux-input-headers = self.callPackage ./pkgs/linux-input-headers.nix { };
    # Imported, not callPackage'd: gtk4 calls libepoxy.override { x11Support = ...; },
    # which must stay nixpkgs' own override.
    libepoxy = import ./pkgs/libepoxy.nix { inherit (prev) libepoxy libglvnd; };
    gtk4 = self.callPackage ./pkgs/gtk4.nix { inherit (prev) gtk4; };
    gtk4-demos = self.callPackage ./pkgs/gtk4-demos.nix { };

    compositor = self.callPackage ./pkgs/compositor.nix { };

    # Wayland clients for the session.
    # VTE (GNOME's terminal widget) on our Wayland GTK 4, with its sample terminal app.
    vte =
      (prev.vte.override {
        gtkVersion = "4";
        gtk4 = self.gtk4;
        withApp = true;
      }).overrideAttrs
        (old: {
          patches = (old.patches or [ ]) ++ [ ../patches/vte/001-darwin-fdwalk-stack.patch ];
        });

    wl-spike = self.callPackage ../spikes/m0-wayland { };
    wl-spike-test = self.callPackage ../spikes/m0-wayland/test.nix { };
  });
}
