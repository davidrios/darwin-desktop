# GTK's demo programs, wrapped with the runtime environment they need on Darwin.
#
# On Linux nixpkgs wraps these with wrapGAppsHook; on Darwin it doesn't, so they
# start without GSettings schemas, without an SVG pixbuf loader (GTK aborts on
# its own SVG icons) and without GStreamer's playback plugin (the widget
# factory's video widget aborts). The wrappers set those three. They don't set
# WAYLAND_DISPLAY / XDG_RUNTIME_DIR: point them at a compositor first.
{
  lib,
  runCommand,
  makeWrapper,
  gtk4,
  librsvg,
  gst_all_1,
}:

let
  schemas = "${gtk4}/share/gsettings-schemas/${gtk4.name}/glib-2.0/schemas";
  programs = [
    "gtk4-demo"
    "gtk4-demo-application"
    "gtk4-widget-factory"
    "gtk4-node-editor"
    "gtk4-print-editor"
  ];
in
runCommand "gtk4-wayland-demos-${gtk4.version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    meta = {
      description = "GTK 4 demos (Wayland backend) wrapped for Darwin";
      platforms = lib.platforms.darwin;
      mainProgram = "gtk4-widget-factory";
    };
  }
  ''
    mkdir -p $out/bin
    test -d ${schemas}
    ${lib.concatMapStringsSep "\n" (p: ''
      makeWrapper ${lib.getDev gtk4}/bin/${p} $out/bin/${p} \
        --set-default GSETTINGS_SCHEMA_DIR ${schemas} \
        --set-default GDK_PIXBUF_MODULE_FILE ${librsvg}/lib/gdk-pixbuf-2.0/2.10.0/loaders.cache \
        --prefix GST_PLUGIN_SYSTEM_PATH_1_0 : ${gst_all_1.gst-plugins-base}/lib/gstreamer-1.0:${gst_all_1.gstreamer.out}/lib/gstreamer-1.0
    '') programs}
  ''
