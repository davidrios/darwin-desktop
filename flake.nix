{
  description = "Darwin Wayland desktop: a Hyprland-style Wayland session on Apple Silicon macOS";

  inputs = {
    # Include the third_party/ submodules (Smithay, reis) in the flake's source.
    self.submodules = true;
    # Pinned exactly through flake.lock. Update deliberately with `nix flake update`.
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs =
    { self, nixpkgs }:
    let
      # Apple Silicon only, by design (see docs/PROJECT_OUTLINE.md).
      system = "aarch64-darwin";
      pkgs = import nixpkgs {
        inherit system;
        overlays = [ self.overlays.default ];
      };
      dw = pkgs.darwinWayland;
    in
    {
      overlays.default = import ./nix/overlay.nix;

      packages.${system} = {
        inherit (dw)
          wayland
          wl-spike
          libepoxy
          gtk4
          gtk4-demos
          gtk3
          gtk-layer-shell
          waybar
          wl-clipboard
          dunst
          pulseaudio
          pamixer
          compositor
          vte
          session
          ;
        default = dw.compositor;
      };

      apps.${system}.session = {
        type = "app";
        program = "${dw.session}/bin/darwin-desktop-session";
      };

      checks.${system} = {
        wl-spike = dw.wl-spike-test;
        compositor = dw.compositor; # builds and runs its unit tests
      };

      devShells.${system}.default = pkgs.mkShell {
        name = "darwin-desktop";
        packages = [
          dw.wayland
          dw.wayland-scanner
          dw.wayland-protocols
          pkgs.epoll-shim
          pkgs.pkg-config
          pkgs.meson
          pkgs.ninja
          pkgs.cmake
          # compositor/ (Rust, Smithay)
          pkgs.cargo
          pkgs.rustc
          pkgs.clippy
          pkgs.rustfmt
          pkgs.rust-analyzer
          pkgs.libxkbcommon
        ];
        # Rust links libxkbcommon by name; point the linker at nixpkgs' copy.
        LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.libxkbcommon ];
        shellHook = ''
          # libwayland falls back to TMPDIR when XDG_RUNTIME_DIR is unset (Darwin patch),
          # but a dedicated directory keeps sockets tidy.
          export XDG_RUNTIME_DIR="''${XDG_RUNTIME_DIR:-''${TMPDIR%/}/darwin-desktop-runtime}"
          mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
        '';
      };

      formatter.${system} = pkgs.nixfmt-rfc-style;
    };
}
