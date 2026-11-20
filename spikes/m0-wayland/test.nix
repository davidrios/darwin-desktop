# `nix flake check` runs the server/client pair inside the build.
{ runCommand, wl-spike }:

runCommand "wl-spike-test" { nativeBuildInputs = [ wl-spike ]; } ''
  export XDG_RUNTIME_DIR="$TMPDIR/rt"
  mkdir -p "$XDG_RUNTIME_DIR"
  run-spike.sh | tee "$out"
''
