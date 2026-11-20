# Linux evdev event codes (KEY_*, BTN_*, ABS_*) as headers for Darwin.
#
# Wayland's input events carry evdev codes (wl_pointer.button is BTN_LEFT and
# friends), so Wayland clients and toolkits include <linux/input.h> or
# <linux/input-event-codes.h> for the constants. Only the constants are
# provided: <linux/input.h> is a shim that includes the codes, without the
# kernel's ioctl structures. FreeBSD solves the same problem with evdev-proto.
#
# The header is "GPL-2.0 WITH Linux-syscall-note", which permits use by
# programs of any licence.
{
  lib,
  stdenvNoCC,
  fetchurl,
}:

stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "linux-input-headers";
  version = "6.12";

  src = fetchurl {
    url = "https://raw.githubusercontent.com/torvalds/linux/v${finalAttrs.version}/include/uapi/linux/input-event-codes.h";
    hash = "sha256-+Yiac9fU9+MD0tNGo2ImVxacXo19NUy3oDD/R6f9/e4=";
  };

  dontUnpack = true;

  installPhase = ''
    install -Dm644 $src $out/include/linux/input-event-codes.h
    cat > $out/include/linux/input.h <<'EOF'
    /* Darwin shim: evdev constants only, no kernel ioctl interface. */
    #ifndef _DARWIN_DESKTOP_LINUX_INPUT_H
    #define _DARWIN_DESKTOP_LINUX_INPUT_H
    #include <linux/input-event-codes.h>
    #endif
    EOF
  '';

  meta = {
    description = "Linux evdev input event codes for non-Linux Wayland builds";
    license = lib.licenses.gpl2Only; # WITH Linux-syscall-note
    platforms = lib.platforms.all;
  };
})
