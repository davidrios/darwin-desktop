# `nix run .#session`: the compositor with the environment its clients need.
#
#   XDG_CONFIG_DIRS session defaults for GTK (gtk-4.0/settings.ini): only a
#                   close button in header bars, as under Hyprland. GTK 4 on
#                   Wayland takes settings from the xdg-desktop-portal Settings
#                   portal; without one (no D-Bus session yet) it falls back to
#                   its built-in "menu:minimize,maximize,close". A user's own
#                   ~/.config/gtk-4.0/settings.ini still wins.
#   XDG_DATA_DIRS   GSettings schemas: GTK's and gsettings-desktop-schemas
#   GDK_PIXBUF_MODULE_FILE, GST_PLUGIN_SYSTEM_PATH_1_0   SVG icons, media
#   TERMINAL, PATH  VTE's terminal and the GTK demos
#
# On Linux, wrapGAppsHook and the desktop session provide these; here the
# session does. COMPOSITOR=path runs a development build instead.
{
  lib,
  writeShellApplication,
  writeTextDir,
  compositor,
  vte,
  gtk4,
  gtk4-demos,
  gsettings-desktop-schemas,
  librsvg,
  gst_all_1,
}:

let
  schemas = pkg: "${pkg}/share/gsettings-schemas/${pkg.name}";
  gtkDefaults = writeTextDir "gtk-4.0/settings.ini" ''
    [Settings]
    gtk-decoration-layout=appmenu:close
  '';
in
writeShellApplication {
  name = "darwin-desktop-session";
  runtimeInputs = [
    compositor
    vte
    gtk4-demos
  ];
  text = ''
    export XDG_CONFIG_DIRS="${gtkDefaults}:''${XDG_CONFIG_DIRS:-/etc/xdg}"
    export XDG_DATA_DIRS="${schemas gsettings-desktop-schemas}:${schemas gtk4}:${vte}/share:${gtk4}/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
    export GDK_PIXBUF_MODULE_FILE="${librsvg}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
    export GST_PLUGIN_SYSTEM_PATH_1_0="${gst_all_1.gst-plugins-base}/lib/gstreamer-1.0:${gst_all_1.gstreamer.out}/lib/gstreamer-1.0"
    export TERMINAL="''${TERMINAL:-vte-2.91-gtk4}"
    exec "''${COMPOSITOR:-compositor}" "$@"
  '';
  meta = {
    description = "Run the compositor with a session environment for its clients";
    platforms = lib.platforms.darwin;
  };
}
