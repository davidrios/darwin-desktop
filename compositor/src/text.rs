//! Text for the compositor's own drawing (group bar titles): one line laid
//! out with Core Text and drawn into a BGRA bitmap (premultiplied, the
//! renderer's texture format), centred, truncated with an ellipsis to fit.

use std::{ffi::c_void, ptr};

use objc2_core_foundation::{CFAttributedString, CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::{CGBitmapContextCreate, CGColorSpace, CGContext};
use objc2_core_text::{CTFont, CTLine, CTLineTruncationType, kCTFontAttributeName, kCTForegroundColorFromContextAttributeName};

/// kCGImageAlphaPremultipliedFirst | kCGBitmapByteOrder32Little: BGRA in memory.
const BGRA_PREMULTIPLIED: u32 = 2 | (2 << 12);

/// A rendered line of text.
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    /// BGRA, premultiplied, rows top to bottom.
    pub pixels: Vec<u8>,
}

/// `text` in `font` (a family or font name; None: the system UI font) at
/// `size_px`, centred in a `width`×`height` pixel bitmap, in `color`
/// (straight RGBA).
pub fn render_line(text: &str, font: Option<&str>, size_px: f64, width: usize, height: usize, color: [f32; 4]) -> Option<Bitmap> {
    if width == 0 || height == 0 {
        return None;
    }
    let name = CFString::from_str(font.filter(|f| !f.is_empty()).unwrap_or(".AppleSystemUIFont"));
    // SAFETY: a null matrix is allowed.
    let font = unsafe { CTFont::with_name(&name, size_px, ptr::null()) };
    let line = line_for(text, &font)?;
    let ellipsis = line_for("…", &font);
    // SAFETY: plain Core Text calls on valid objects.
    let line = unsafe { line.truncated_line(width as f64, CTLineTruncationType::End, ellipsis.as_deref()) }.unwrap_or(line);
    let (mut ascent, mut descent, mut leading) = (0.0, 0.0, 0.0);
    // SAFETY: the out pointers are valid locals.
    let line_width = unsafe { line.typographic_bounds(&mut ascent, &mut descent, &mut leading) };

    let mut pixels = vec![0u8; width * height * 4];
    let space = CGColorSpace::new_device_rgb()?;
    // SAFETY: `pixels` outlives the context and is width*height*4 bytes.
    let ctx = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr() as *mut c_void,
            width,
            height,
            8,
            width * 4,
            Some(&space),
            BGRA_PREMULTIPLIED,
        )
    }?;
    let [r, g, b, a] = color.map(|c| c as f64);
    CGContext::set_rgb_fill_color(Some(&ctx), r, g, b, a);
    // Core Graphics' origin is the bottom left: centre the line on its baseline box.
    let x = ((width as f64 - line_width) / 2.0).max(0.0);
    let y = ((height as f64 - (ascent + descent)) / 2.0 + descent).round();
    CGContext::set_text_position(Some(&ctx), x.round(), y);
    // SAFETY: the context and line are valid.
    unsafe { line.draw(&ctx) };
    drop(ctx);
    Some(Bitmap { width, height, pixels })
}

/// A line of `text` in `font`, coloured by the context's fill colour.
fn line_for(text: &str, font: &CTFont) -> Option<CFRetained<CTLine>> {
    let string = CFString::from_str(text);
    let from_context = CFBoolean::new(true);
    // SAFETY: the attribute names are Core Text's constants.
    let keys: [&CFString; 2] = unsafe { [kCTFontAttributeName, kCTForegroundColorFromContextAttributeName] };
    let values: [&CFType; 2] = [font.as_ref(), from_context.as_ref()];
    let attrs = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    // SAFETY: the dictionary holds CFString keys and CFType values.
    let attributed = unsafe { CFAttributedString::new(None, Some(&string), Some(attrs.as_opaque())) }?;
    // SAFETY: a valid attributed string.
    Some(unsafe { CTLine::with_attributed_string(&attributed) })
}

#[cfg(test)]
mod tests {
    #[test]
    fn draws_some_pixels() {
        let bmp = super::render_line("Terminal — vim", None, 24.0, 200, 32, [1.0, 1.0, 1.0, 1.0]).unwrap();
        assert_eq!(bmp.pixels.len(), 200 * 32 * 4);
        let lit = bmp.pixels.chunks(4).filter(|p| p[3] > 0).count();
        assert!(lit > 100, "only {lit} pixels drawn");
        // A long title is cut to fit, not drawn past the edges.
        let long = "x".repeat(500);
        let bmp = super::render_line(&long, None, 24.0, 100, 32, [1.0, 1.0, 1.0, 1.0]).unwrap();
        assert!(bmp.pixels.chunks(4).any(|p| p[3] > 0));
    }
}
