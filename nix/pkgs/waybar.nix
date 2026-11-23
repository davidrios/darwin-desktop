# Waybar on our Wayland GTK 3, for Darwin.
#
# nixpkgs' waybar with every feature that needs Linux (or a Linux-only
# library) off: audio (PulseAudio, PipeWire, WirePlumber, JACK, sndio, cava),
# input devices (libinput, libevdev, udev: backlight, keyboard-state), network
# (libnl), power (UPower, logind, systemd), MPD/MPRIS, GPS, rfkill, WWAN and the
# tray (needs D-Bus and libdbusmenu). The niri, mango and umbriel IPC modules
# are off too. What's left: clock, custom, image, disk, load, temperature,
# user, idle-inhibitor, the wlr/ext Wayland modules, and the Hyprland, Sway,
# River, dwl and Wayfire IPC modules (Hyprland's needs our compositor to speak
# Hyprland's IPC; it doesn't yet). docs/waybar-notes.md.
#
# Not wrapped (no wrapGAppsHook3: it would pull in nixpkgs' Quartz GTK 3); the
# session provides the GTK environment, as for the other apps.
{
  lib,
  waybar,
  gtkmm3,
  gtk-layer-shell,
  wayland,
  howard-hinnant-date,
}:

(waybar.override {
  inherit gtkmm3 gtk-layer-shell wayland;
  wrapGAppsHook3 = null;
  cavaSupport = false;
  enableManpages = false;
  evdevSupport = false;
  gpsSupport = false;
  inputSupport = false;
  jackSupport = false;
  mangoSupport = false;
  mpdSupport = false;
  mprisSupport = false;
  niriSupport = false;
  nlSupport = false;
  pipewireSupport = false;
  pulseSupport = false;
  rfkillSupport = false;
  sndioSupport = false;
  systemdSupport = false;
  traySupport = false;
  udevSupport = false;
  umbrielSupport = false;
  upowerSupport = false;
  wireplumberSupport = false;
  wwanSupport = false;
}).overrideAttrs
  (old: {
    pname = "waybar-wayland";

    patches = (old.patches or [ ]) ++ [
      ../../patches/waybar/001-darwin.patch
      ../../patches/waybar/002-desktop-app-info.patch
    ];

    # This libc++ has no std::chrono time zones, so waybar wants Howard
    # Hinnant's date library for its clock; without it waybar falls back to
    # simpleclock, which no longer compiles with fmt 12 (fmt::localtime).
    buildInputs = old.buildInputs ++ [ howard-hinnant-date ];

    mesonFlags = old.mesonFlags ++ [ (lib.mesonEnable "logind" false) ];

    meta = old.meta // {
      platforms = old.meta.platforms ++ [ "aarch64-darwin" ];
    };
  })
