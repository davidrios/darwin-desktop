# libepoxy with its EGL dispatch built on Darwin.
#
# GTK's Wayland backend includes <epoxy/egl.h> unconditionally. epoxy 1.5.10
# hard-codes EGL off on Apple; upstream commit 47d975d (2025-10-29, "Improve
# library detections", unreleased as of 1.5.10) lets -Degl=yes work there and
# loads libEGL.dylib (e.g. ANGLE) at runtime. Without a libEGL,
# epoxy_has_egl() returns false when there is none, which GTK checks before
# using EGL. So with no libEGL installed, GTK's renderer falls back to cairo
# (fine for wl_shm clients until M4 brings a Darwin EGL/Vulkan WSI).
#
# The Khronos EGL headers come from libglvnd's dev output. Nothing is linked,
# but epoxy's public <epoxy/egl.h> includes <EGL/eglplatform.h>, so the headers
# are propagated to everything that builds against epoxy.
{ libepoxy, libglvnd }:

(libepoxy.override { x11Support = false; }).overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [ ../../patches/libepoxy/001-egl-on-darwin.patch ];
  propagatedBuildInputs = (old.propagatedBuildInputs or [ ]) ++ [ libglvnd.dev ];
  mesonFlags = map (f: if f == "-Degl=no" then "-Degl=yes" else f) old.mesonFlags;
})
