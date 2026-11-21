//! The client's cursor as an `NSCursor`.
//!
//! Named shapes (cursor-shape protocol, `wp_cursor_shape_v1`) map to macOS's
//! own cursors; cursor surfaces (`wl_pointer.set_cursor`) become an image
//! cursor with the client's hotspot. The shape mapping follows mitsuami's
//! `system_cursor`: macOS 15's frame-resize, column/row-resize and zoom
//! cursors where present, looked up at run time.

use objc2::{AnyThread, ClassType, rc::Retained, sel};
use objc2_app_kit::{
    NSBitmapFormat, NSBitmapImageRep, NSCursor, NSCursorFrameResizeDirections, NSCursorFrameResizePosition,
    NSDeviceRGBColorSpace, NSImage,
};
use objc2_foundation::{NSPoint, NSSize};
use smithay::{
    backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData},
    input::pointer::{CursorIcon, CursorImageStatus, CursorImageSurfaceData},
    reexports::wayland_server::{Resource, backend::ObjectId, protocol::wl_shm},
    wayland::{compositor::with_states, shm::with_buffer_contents},
};

/// What the cursor currently shows; a change means building a new `NSCursor`.
#[derive(Clone, PartialEq, Debug)]
pub enum CursorKey {
    Hidden,
    Named(CursorIcon),
    Surface(ObjectId, Option<CommitCounter>),
}

pub fn key(status: &CursorImageStatus) -> CursorKey {
    match status {
        CursorImageStatus::Hidden => CursorKey::Hidden,
        CursorImageStatus::Named(icon) => CursorKey::Named(*icon),
        CursorImageStatus::Surface(surface) => {
            let commit = with_states(surface, |states| {
                states.data_map.get::<RendererSurfaceStateUserData>().map(|d| d.lock().unwrap().current_commit())
            });
            CursorKey::Surface(surface.id(), commit)
        }
    }
}

/// The `NSCursor` for a status; `None` means the arrow.
pub fn ns_cursor(status: &CursorImageStatus) -> Option<Retained<NSCursor>> {
    match status {
        CursorImageStatus::Hidden => transparent(),
        CursorImageStatus::Named(icon) => named(*icon),
        CursorImageStatus::Surface(surface) => {
            if !surface.is_alive() {
                return None;
            }
            with_states(surface, |states| {
                let hotspot = states
                    .data_map
                    .get::<CursorImageSurfaceData>()
                    .map(|d| d.lock().unwrap().hotspot)
                    .unwrap_or_default();
                let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
                let data = data.lock().unwrap();
                // A cursor surface without a buffer hides the cursor.
                let Some(buffer) = data.buffer() else { return transparent() };
                let scale = data.buffer_scale().max(1) as f64;
                with_buffer_contents(buffer, |ptr, len, info| {
                    let alpha = match info.format {
                        wl_shm::Format::Argb8888 => true,
                        wl_shm::Format::Xrgb8888 => false,
                        _ => return None,
                    };
                    let (w, h, stride, offset) = (info.width, info.height, info.stride, info.offset);
                    if w <= 0 || h <= 0 || stride <= 0 || offset < 0 || w > 1024 || h > 1024 {
                        return None;
                    }
                    let row = w as usize * 4;
                    let end = (stride as usize).checked_mul(h as usize - 1)?.checked_add(offset as usize)?.checked_add(row)?;
                    if (stride as usize) < row || end > len {
                        return None;
                    }
                    // SAFETY: the extent was checked against the pool's length.
                    let pixels = unsafe { std::slice::from_raw_parts(ptr.add(offset as usize), end - offset as usize) };
                    let hotspot = NSPoint::new(hotspot.x as f64, hotspot.y as f64);
                    image_cursor(pixels, w as usize, h as usize, stride as usize, alpha, scale, hotspot)
                })
                .ok()
                .flatten()
            })
        }
    }
}

/// An image cursor from premultiplied BGRA rows (wl_shm's little-endian
/// ARGB8888, which is also AppKit's 32-bit little-endian alpha-first format).
fn image_cursor(
    pixels: &[u8],
    width: usize,
    height: usize,
    stride: usize,
    alpha: bool,
    scale: f64,
    hotspot: NSPoint,
) -> Option<Retained<NSCursor>> {
    // SAFETY: AppKit allocates the planes (null) and owns them.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::AlphaFirst | NSBitmapFormat::ThirtyTwoBitLittleEndian,
            (width * 4) as isize,
            32,
        )
    }?;
    let dst = rep.bitmapData();
    if dst.is_null() {
        return None;
    }
    for y in 0..height {
        let src = &pixels[y * stride..y * stride + width * 4];
        // SAFETY: the rep's buffer is width*4 bytes per row, height rows.
        let out = unsafe { std::slice::from_raw_parts_mut(dst.add(y * width * 4), width * 4) };
        out.copy_from_slice(src);
        if !alpha {
            for px in out.chunks_exact_mut(4) {
                px[3] = 0xff; // XRGB: B, G, R, X in memory
            }
        }
    }
    // The image's size is in points: a scale-2 buffer is half its pixels.
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(width as f64 / scale, height as f64 / scale));
    image.addRepresentation(&rep);
    Some(NSCursor::initWithImage_hotSpot(NSCursor::alloc(), &image, hotspot))
}

fn transparent() -> Option<Retained<NSCursor>> {
    image_cursor(&[0, 0, 0, 0], 1, 1, 4, true, 1.0, NSPoint::new(0.0, 0.0))
}

/// macOS 15's cursors, when this macOS has them.
fn has_class_method(sel: objc2::runtime::Sel) -> bool {
    NSCursor::class().class_method(sel).is_some()
}

fn frame_resize(position: NSCursorFrameResizePosition) -> Option<Retained<NSCursor>> {
    has_class_method(sel!(frameResizeCursorFromPosition:inDirections:))
        .then(|| NSCursor::frameResizeCursorFromPosition_inDirections(position, NSCursorFrameResizeDirections::All))
}

#[allow(deprecated)]
fn named(icon: CursorIcon) -> Option<Retained<NSCursor>> {
    use NSCursorFrameResizePosition as P;
    Some(match icon {
        CursorIcon::Default | CursorIcon::Help | CursorIcon::Progress | CursorIcon::Wait => return None,
        CursorIcon::ContextMenu => NSCursor::contextualMenuCursor(),
        CursorIcon::Pointer => NSCursor::pointingHandCursor(),
        CursorIcon::Cell | CursorIcon::Crosshair => NSCursor::crosshairCursor(),
        CursorIcon::Text => NSCursor::IBeamCursor(),
        CursorIcon::VerticalText => NSCursor::IBeamCursorForVerticalLayout(),
        CursorIcon::Alias => NSCursor::dragLinkCursor(),
        CursorIcon::Copy | CursorIcon::DndAsk => NSCursor::dragCopyCursor(),
        CursorIcon::NoDrop | CursorIcon::NotAllowed => NSCursor::operationNotAllowedCursor(),
        CursorIcon::Grab | CursorIcon::Move | CursorIcon::AllScroll | CursorIcon::AllResize => {
            NSCursor::openHandCursor()
        }
        CursorIcon::Grabbing => NSCursor::closedHandCursor(),
        CursorIcon::NResize | CursorIcon::SResize | CursorIcon::NsResize => {
            frame_resize(P::Top).unwrap_or_else(NSCursor::resizeUpDownCursor)
        }
        CursorIcon::EResize | CursorIcon::WResize | CursorIcon::EwResize => {
            frame_resize(P::Left).unwrap_or_else(NSCursor::resizeLeftRightCursor)
        }
        CursorIcon::NeResize | CursorIcon::SwResize | CursorIcon::NeswResize => frame_resize(P::TopRight)?,
        CursorIcon::NwResize | CursorIcon::SeResize | CursorIcon::NwseResize => frame_resize(P::TopLeft)?,
        CursorIcon::ColResize if has_class_method(sel!(columnResizeCursor)) => NSCursor::columnResizeCursor(),
        CursorIcon::ColResize => NSCursor::resizeLeftRightCursor(),
        CursorIcon::RowResize if has_class_method(sel!(rowResizeCursor)) => NSCursor::rowResizeCursor(),
        CursorIcon::RowResize => NSCursor::resizeUpDownCursor(),
        CursorIcon::ZoomIn if has_class_method(sel!(zoomInCursor)) => NSCursor::zoomInCursor(),
        CursorIcon::ZoomOut if has_class_method(sel!(zoomOutCursor)) => NSCursor::zoomOutCursor(),
        _ => return None,
    })
}
