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

    wl-spike = self.callPackage ../spikes/m0-wayland { };
    wl-spike-test = self.callPackage ../spikes/m0-wayland/test.nix { };
  });
}
