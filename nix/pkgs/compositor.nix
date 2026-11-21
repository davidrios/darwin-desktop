# The compositor (compositor/), built from the patched Smithay and reis
# submodules. The flake includes submodules in its source (inputs.self.submodules,
# pristine: nothing patches them in place), and the same queues
# scripts/prepare-third-party.sh applies are applied here, so pins and patches
# have one source of truth.
{
  lib,
  rustPlatform,
  pkg-config,
  libxkbcommon,
}:

let
  root = ../..;
  queues = [
    "smithay"
    "reis"
  ];
in
rustPlatform.buildRustPackage {
  pname = "compositor";
  version = "0.1.0";

  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions (
      [
        (root + "/compositor/Cargo.toml")
        (root + "/compositor/Cargo.lock")
        (root + "/compositor/src")
      ]
      ++ map (q: root + "/third_party/${q}") queues
      ++ map (q: root + "/patches/${q}") queues
    );
  };

  cargoRoot = "compositor";
  buildAndTestSubdir = "compositor";
  cargoLock.lockFile = root + "/compositor/Cargo.lock";

  # The same layout scripts/prepare-third-party.sh makes: the pristine
  # submodule copied to build/third_party/<name>, then its queue applied.
  postPatch = lib.concatMapStrings (q: ''
    mkdir -p build/third_party
    cp -R third_party/${q} build/third_party/${q}
    chmod -R u+w build/third_party/${q}
    for p in patches/${q}/*.patch; do
      echo "applying $p"
      patch -p1 --no-backup-if-mismatch -d build/third_party/${q} < "$p"
    done
  '') queues;

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ libxkbcommon ];

  meta = {
    description = "Hyprland-style Wayland compositor for macOS (working title)";
    license = with lib.licenses; [
      mit
      asl20
    ];
    platforms = [ "aarch64-darwin" ];
    mainProgram = "compositor";
  };
}
