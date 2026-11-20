{
  lib,
  stdenv,
  pkg-config,
  wayland,
}:

stdenv.mkDerivation {
  pname = "wl-spike";
  version = "0.1.0";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Makefile
      ./common.h
      ./server.c
      ./client.c
      ./run-spike.sh
    ];
  };

  nativeBuildInputs = [ pkg-config ];
  buildInputs = [ wayland ];

  makeFlags = [ "PREFIX=${placeholder "out"}" ];

  meta = {
    description = "M0 spike: minimal Wayland server and wl_shm client on Darwin";
    platforms = lib.platforms.darwin;
    mainProgram = "run-spike.sh";
  };
}
