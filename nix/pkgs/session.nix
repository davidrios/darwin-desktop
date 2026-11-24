# `nix run .#session`: the compositor with the environment its clients need.
#
#   XDG_CONFIG_DIRS session defaults for GTK (gtk-{3,4}.0/settings.ini): only a
#                   close button in header bars, as under Hyprland. GTK 4 on
#                   Wayland takes settings from the xdg-desktop-portal Settings
#                   portal; without one (no D-Bus session yet) it falls back to
#                   its built-in "menu:minimize,maximize,close". A user's own
#                   ~/.config/gtk-4.0/settings.ini still wins.
#   WAYBAR_CONFIG_DIR  a waybar config of modules that work here (Hyprland
#                   IPC, clock, disk, load), with waybar's own style.css. Set
#                   only without ~/.config/waybar: waybar reads this variable
#                   first and ignores XDG_CONFIG_DIRS.
#   XDG_DATA_DIRS   GSettings schemas: GTK's and gsettings-desktop-schemas;
#                   icon themes (Adwaita, hicolor) for GTK 3, which has none built in
#   GDK_PIXBUF_MODULE_FILE, GST_PLUGIN_SYSTEM_PATH_1_0   SVG icons, media
#   TERMINAL, PATH  VTE's terminal, the GTK demos, waybar and gtk-layer-demo
#
# On Linux, wrapGAppsHook and the desktop session provide these; here the
# session does. COMPOSITOR=path runs a development build instead.
{
  lib,
  writeShellApplication,
  writeTextDir,
  symlinkJoin,
  compositor,
  vte,
  gtk4,
  gtk4-demos,
  gtk3,
  gtk-layer-shell,
  waybar,
  adwaita-icon-theme,
  hicolor-icon-theme,
  gsettings-desktop-schemas,
  librsvg,
  gst_all_1,
}:

let
  schemas = pkg: "${pkg}/share/gsettings-schemas/${pkg.name}";
  settings = ''
    [Settings]
    gtk-decoration-layout=appmenu:close
  '';
  waybarConfig = {
    layer = "top";
    position = "top";
    height = 30;
    modules-left = [ "hyprland/workspaces" ];
    modules-center = [ "hyprland/window" ];
    modules-right = [
      "hyprland/language"
      "disk"
      "load"
      "clock"
    ];
    "hyprland/window".max-length = 60;
    disk = {
      path = "/";
      format = "{percentage_used}% disk";
    };
    load.format = "load {load1}";
    clock = {
      format = "{:%a %d %b  %H:%M}";
      tooltip-format = "<tt>{calendar}</tt>";
    };
  };
  waybarDefaults = writeTextDir "config.jsonc" (builtins.toJSON waybarConfig);
  gtkDefaults = symlinkJoin {
    name = "gtk-session-defaults";
    paths = [
      (writeTextDir "gtk-4.0/settings.ini" settings)
      (writeTextDir "gtk-3.0/settings.ini" settings)
    ];
  };
in
writeShellApplication {
  name = "darwin-desktop-session";
  runtimeInputs = [
    compositor
    vte
    gtk4-demos
    waybar
    gtk-layer-shell.dev
  ];
  text = ''
    export XDG_CONFIG_DIRS="${gtkDefaults}:''${XDG_CONFIG_DIRS:-/etc/xdg}"
    export XDG_DATA_DIRS="${schemas gsettings-desktop-schemas}:${schemas gtk4}:${schemas gtk3}:${vte}/share:${gtk4}/share:${gtk3}/share:${adwaita-icon-theme}/share:${hicolor-icon-theme}/share''${XDG_DATA_DIRS:+:$XDG_DATA_DIRS}"
    export GDK_PIXBUF_MODULE_FILE="${librsvg}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache"
    export GST_PLUGIN_SYSTEM_PATH_1_0="${gst_all_1.gst-plugins-base}/lib/gstreamer-1.0:${gst_all_1.gstreamer.out}/lib/gstreamer-1.0"
    if [ -z "''${WAYBAR_CONFIG_DIR:-}" ] && [ ! -e "''${XDG_CONFIG_HOME:-$HOME/.config}/waybar" ]; then
      export WAYBAR_CONFIG_DIR="${waybarDefaults}"
    fi
    export TERMINAL="''${TERMINAL:-vte-2.91-gtk4}"
    exec "''${COMPOSITOR:-compositor}" "$@"
  '';
  meta = {
    description = "Run the compositor with a session environment for its clients";
    platforms = lib.platforms.darwin;
  };
}
