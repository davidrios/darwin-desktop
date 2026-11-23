//! Metal renderer: each client surface's shm buffer becomes a texture, drawn
//! as a quad into the window's `CAMetalLayer`.
//!
//! M1 uploads a surface's whole buffer whenever it has a new commit and
//! redraws the whole frame; damage tracking comes later.
//!
//! Windows are drawn where their animations are (`anim`): a window's content
//! is laid out at its goal rectangle and mapped into the animated one, scaled
//! and moved, with its border, rounding and alpha following.

use std::{cell::RefCell, ffi::c_void, ptr::NonNull};

use objc2::{rc::Retained, runtime::ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue, MTLCreateSystemDefaultDevice,
    MTLDevice, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat, MTLPrimitiveType, MTLRegion,
    MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor, MTLRenderPipelineState, MTLScissorRect,
    MTLSize,
    MTLStoreAction, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};
use smithay::{
    backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData},
    desktop::PopupManager,
    reexports::wayland_server::protocol::{wl_shm, wl_surface::WlSurface},
    utils::{Logical, Point, Rectangle, Size},
    wayland::{
        compositor::{SurfaceData, TraversalAction, with_surface_tree_downward},
        shm::with_buffer_contents,
    },
};

use crate::{anim, config::Gradient, config::Settings, dwindle::Rect, state::State};

const SHADERS: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Quad {
    float4 dst;        // x, y, w, h in normalized device coordinates
    float4 src;        // u, v, w, h in texture coordinates
    float4 rect;       // rounded rectangle in pixels (x, y, w, h), top-left origin
    float alpha;
    uint opaque;       // XRGB: ignore the alpha channel
    float radius;      // corner radius in pixels; 0: no rounding
    float border;      // border width in pixels (border pipeline)
    float angle;       // gradient angle in radians (border pipeline)
    uint ncolors;      // gradient colours used (border pipeline)
    float2 _pad;
    float4 colors[8];  // premultiplied gradient colours (border pipeline)
};

struct VOut {
    float4 pos [[position]];
    float2 uv;
};

vertex VOut quad_vertex(uint vid [[vertex_id]], constant Quad &q [[buffer(0)]]) {
    float2 corner = float2(vid & 1, vid >> 1);
    VOut o;
    o.pos = float4(q.dst.xy + corner * q.dst.zw, 0.0, 1.0);
    o.uv = q.src.xy + corner * q.src.zw;
    return o;
}

// Signed distance to a rounded rectangle: negative inside.
static float rounded_rect(float2 p, float4 rect, float r) {
    float2 half_size = rect.zw * 0.5;
    float2 q = abs(p - (rect.xy + half_size)) - (half_size - r);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

fragment float4 quad_fragment(VOut in [[stage_in]], constant Quad &q [[buffer(0)]],
                              texture2d<float> tex [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float4 c = tex.sample(s, in.uv);
    if (q.opaque != 0) { c.a = 1.0; }
    if (q.radius > 0.0) {
        // Antialiased rounded corners: one pixel of coverage falloff.
        c *= saturate(0.5 - rounded_rect(in.pos.xy, q.rect, q.radius));
    }
    return c * q.alpha;
}

// A border: the ring between the outer rounded rectangle and the inner one
// (inset by the border width), filled with an angled gradient.
fragment float4 border_fragment(VOut in [[stage_in]], constant Quad &q [[buffer(0)]]) {
    float2 p = in.pos.xy;
    float outer = rounded_rect(p, q.rect, q.radius);
    float4 inner_rect = q.rect + float4(q.border, q.border, -2.0 * q.border, -2.0 * q.border);
    float inner = rounded_rect(p, inner_rect, max(q.radius - q.border, 0.0));
    float coverage = saturate(0.5 - outer) * saturate(0.5 + inner);

    float4 color = q.colors[0];
    if (q.ncolors > 1) {
        float2 dir = float2(cos(q.angle), sin(q.angle));
        float2 half_size = q.rect.zw * 0.5;
        float extent = abs(half_size.x * dir.x) + abs(half_size.y * dir.y);
        float t = saturate(dot(p - (q.rect.xy + half_size), dir) / max(extent, 1.0) * 0.5 + 0.5);
        float pos = t * float(q.ncolors - 1);
        uint i = min(uint(floor(pos)), q.ncolors - 2);
        color = mix(q.colors[i], q.colors[i + 1], pos - float(i));
    }
    return color * coverage * q.alpha;
}
"#;

/// Matches `struct Quad` in the shader (16-byte aligned, 208 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct Quad {
    dst: [f32; 4],
    src: [f32; 4],
    rect: [f32; 4],
    alpha: f32,
    opaque: u32,
    radius: f32,
    border: f32,
    angle: f32,
    ncolors: u32,
    _pad: [f32; 2],
    colors: [[f32; 4]; 8],
}

const _: () = assert!(std::mem::size_of::<Quad>() == 208);

impl Quad {
    fn empty() -> Quad {
        Quad {
            dst: [0.0; 4],
            src: [0.0, 0.0, 1.0, 1.0],
            rect: [0.0; 4],
            alpha: 1.0,
            opaque: 0,
            radius: 0.0,
            border: 0.0,
            angle: 0.0,
            ncolors: 0,
            _pad: [0.0; 2],
            colors: [[0.0; 4]; 8],
        }
    }
}

type Texture = Retained<ProtocolObject<dyn MTLTexture>>;

/// A surface's uploaded buffer, kept in the surface's data map.
struct SurfaceTexture {
    commit: CommitCounter,
    texture: Texture,
    opaque: bool,
}

pub struct Renderer {
    device: Retained<ProtocolObject<dyn MTLDevice>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    border_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    pub layer: Retained<CAMetalLayer>,
}

impl Renderer {
    pub fn new() -> Renderer {
        let device = MTLCreateSystemDefaultDevice().expect("no Metal device");
        tracing::info!("Metal device: {}", device.name());
        let queue = device.newCommandQueue().expect("command queue");

        let library = device
            .newLibraryWithSource_options_error(&NSString::from_str(SHADERS), None)
            .unwrap_or_else(|err| panic!("compiling shaders: {}", err.localizedDescription()));
        let function = |name: &str| library.newFunctionWithName(&NSString::from_str(name)).expect(name);
        let vertex = function("quad_vertex");
        let pipeline_for = |fragment: &str| {
            let desc = MTLRenderPipelineDescriptor::new();
            desc.setVertexFunction(Some(&vertex));
            desc.setFragmentFunction(Some(&function(fragment)));
            // SAFETY: index 0 always exists.
            let attachment = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            // Wayland buffers and our colours are premultiplied.
            attachment.setBlendingEnabled(true);
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            device
                .newRenderPipelineStateWithDescriptor_error(&desc)
                .unwrap_or_else(|err| panic!("render pipeline {fragment}: {}", err.localizedDescription()))
        };
        let pipeline = pipeline_for("quad_fragment");
        let border_pipeline = pipeline_for("border_fragment");

        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);

        Renderer { device, queue, pipeline, border_pipeline, layer }
    }

    /// Draws every window and its popups, back to front.
    pub fn render(&self, state: &State) {
        let scale = state.output_scale;
        let size = state.output_size.to_f64().to_physical(scale);
        if size.w < 1.0 || size.h < 1.0 {
            return;
        }
        let Some(drawable) = self.layer.nextDrawable() else { return };
        let target = drawable.texture();

        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(&target));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setClearColor(MTLClearColor { red: 0.11, green: 0.12, blue: 0.14, alpha: 1.0 });
        color.setStoreAction(MTLStoreAction::Store);

        let Some(commands) = self.queue.commandBuffer() else { return };
        let Some(encoder) = commands.renderCommandEncoderWithDescriptor(&pass) else { return };
        encoder.setRenderPipelineState(&self.pipeline);

        let settings = &state.config.settings;
        let mut frame = Frame {
            renderer: self,
            encoder: &encoder,
            scale,
            width: size.w,
            height: size.h,
            round: None,
            xform: Xform::IDENTITY,
            alpha: 1.0,
        };
        use smithay::wayland::shell::wlr_layer::Layer;
        frame.layers(state, &[Layer::Background, Layer::Bottom]);
        let transition = state.wm.transition.as_ref();
        // A workspace switch: the workspace being left, sliding or fading out.
        if let Some(tr) = transition {
            if let Some(ws) = state.wm.workspaces.get(&tr.from) {
                let mut windows = ws.windows();
                if let Some(fs) = &ws.fullscreen {
                    windows.retain(|w| w != fs);
                    windows.push(fs.clone());
                }
                for window in &windows {
                    let fullscreen = ws.fullscreen.as_ref() == Some(window);
                    frame.window(settings, window, *tr.out_offset.value(), *tr.out_alpha.value(), fullscreen);
                }
            }
        }
        let (offset, alpha) = transition.map(|t| (*t.in_offset.value(), *t.in_alpha.value())).unwrap_or(([0.0, 0.0], 1.0));
        for window in state.space.elements() {
            frame.window(settings, window, offset, alpha, state.is_fullscreen(window));
        }
        for closing in &state.closing {
            frame.decorated(
                settings,
                *closing.rect.value(),
                closing.anchor,
                *closing.alpha.value(),
                closing.decorated,
                closing.clipped,
                &closing.border,
                |f| {
                    for p in &closing.pieces {
                        f.quad(&p.texture, p.opaque, p.pos, p.size, p.src, p.buffer);
                    }
                },
                |_| {},
            );
        }
        frame.layers(state, state.upper_layers());

        encoder.endEncoding();
        commands.presentDrawable(ProtocolObject::from_ref(&*drawable));
        commands.commit();
    }

    /// The surface's texture, uploading its current shm buffer if it changed.
    fn texture_for(&self, states: &SurfaceData) -> Option<(Texture, bool)> {
        let data = states.data_map.get::<RendererSurfaceStateUserData>()?;
        let data = data.lock().unwrap();
        let commit = data.current_commit();
        states.data_map.insert_if_missing(|| RefCell::new(None::<SurfaceTexture>));
        let cache = states.data_map.get::<RefCell<Option<SurfaceTexture>>>().unwrap();
        if let Some(cached) = cache.borrow().as_ref() {
            if cached.commit == commit {
                return Some((cached.texture.clone(), cached.opaque));
            }
        }
        let buffer = data.buffer()?;
        let uploaded = with_buffer_contents(buffer, |ptr, len, info| {
            let opaque = match info.format {
                wl_shm::Format::Xrgb8888 => true,
                wl_shm::Format::Argb8888 => false,
                other => {
                    tracing::warn!("unsupported shm format {other:?}");
                    return None;
                }
            };
            let (w, h, stride, offset) = (info.width, info.height, info.stride, info.offset);
            // Smithay validates buffers at creation, but this read is unsafe:
            // check the whole extent here too, without overflow. Rows are
            // 4 bytes per pixel; the last row needs only `row` bytes.
            if w <= 0 || h <= 0 || stride <= 0 || offset < 0 {
                return None;
            }
            let row = (w as usize).checked_mul(4)?;
            if (stride as usize) < row {
                return None;
            }
            let end = (stride as usize)
                .checked_mul(h as usize - 1)
                .and_then(|n| n.checked_add(offset as usize))
                .and_then(|n| n.checked_add(row))?;
            if end > len {
                return None;
            }
            let reuse = cache.borrow().as_ref().map(|c| c.texture.clone()).filter(|t| {
                t.width() == w as usize && t.height() == h as usize
            });
            let texture = reuse.or_else(|| self.new_texture(w as usize, h as usize))?;
            let region = MTLRegion {
                origin: MTLOrigin { x: 0, y: 0, z: 0 },
                size: MTLSize { width: w as usize, height: h as usize, depth: 1 },
            };
            // SAFETY: the range was checked against the pool's length above,
            // and BGRA8 matches wl_shm's little-endian (A|X)RGB8888.
            unsafe {
                let bytes = NonNull::new_unchecked(ptr.add(offset as usize) as *mut c_void);
                texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, stride as usize);
            }
            Some((texture, opaque))
        });
        let (texture, opaque) = match uploaded {
            Ok(Some(t)) => t,
            Ok(None) => return None,
            Err(err) => {
                tracing::warn!("reading a buffer: {err:?}");
                return None;
            }
        };
        *cache.borrow_mut() = Some(SurfaceTexture { commit, texture: texture.clone(), opaque });
        Some((texture, opaque))
    }

    fn new_texture(&self, width: usize, height: usize) -> Option<Texture> {
        // SAFETY: plain descriptor construction.
        let desc = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width,
                height,
                false,
            )
        };
        desc.setUsage(MTLTextureUsage::ShaderRead);
        self.device.newTextureWithDescriptor(&desc)
    }
}

struct Frame<'a> {
    renderer: &'a Renderer,
    encoder: &'a ProtocolObject<dyn MTLRenderCommandEncoder>,
    scale: f64,
    width: f64,
    height: f64,
    /// The window rectangle and corner radius (logical, on screen) the
    /// surfaces being drawn are rounded to.
    round: Option<(Rect, f64)>,
    /// Where the surfaces being drawn go (a window's animation).
    xform: Xform,
    /// Opacity of what's drawn (a window's fade).
    alpha: f32,
}

/// Maps a window's content from where it's laid out (around `anchor`) to
/// where it's drawn (from `base`, scaled).
#[derive(Clone, Copy)]
struct Xform {
    base: (f64, f64),
    anchor: (f64, f64),
    scale: (f64, f64),
}

impl Xform {
    const IDENTITY: Xform = Xform { base: (0.0, 0.0), anchor: (0.0, 0.0), scale: (1.0, 1.0) };

    fn point(&self, p: Point<f64, Logical>) -> Point<f64, Logical> {
        (self.base.0 + (p.x - self.anchor.0) * self.scale.0, self.base.1 + (p.y - self.anchor.1) * self.scale.1).into()
    }
}

/// A surface's last texture and where it was, kept to draw a closed window
/// animating out.
pub struct Piece {
    texture: Texture,
    opaque: bool,
    pos: Point<f64, Logical>,
    size: Size<f64, Logical>,
    src: Rectangle<f64, Logical>,
    buffer: Size<f64, Logical>,
}

/// The textures of a surface tree as last drawn, laid out from `origin`.
pub fn snapshot(surface: &WlSurface, origin: Point<f64, Logical>) -> Vec<Piece> {
    let mut pieces = Vec::new();
    with_surface_tree_downward(
        surface,
        origin,
        |_, states, loc| {
            let view = states.data_map.get::<RendererSurfaceStateUserData>().and_then(|d| d.lock().unwrap().view());
            match view {
                Some(view) => TraversalAction::DoChildren(*loc + view.offset.to_f64()),
                None => TraversalAction::SkipChildren,
            }
        },
        |_, states, loc| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else { return };
            let (view, buffer_size) = {
                let d = data.lock().unwrap();
                (d.view(), d.buffer_size())
            };
            let (Some(view), Some(buffer)) = (view, buffer_size) else { return };
            let Some(cache) = states.data_map.get::<RefCell<Option<SurfaceTexture>>>() else { return };
            let Some(cached) = cache.borrow().as_ref().map(|c| (c.texture.clone(), c.opaque)) else { return };
            pieces.push(Piece {
                texture: cached.0,
                opaque: cached.1,
                pos: *loc + view.offset.to_f64(),
                size: view.dst.to_f64(),
                src: view.src,
                buffer: buffer.to_f64(),
            });
        },
        |_, _, _| true,
    );
    pieces
}

impl Frame<'_> {
    /// A logical rectangle in normalized device coordinates (y up, height negative).
    fn ndc(&self, x: f64, y: f64, w: f64, h: f64) -> [f32; 4] {
        [
            (x / self.width * 2.0 - 1.0) as f32,
            (1.0 - y / self.height * 2.0) as f32,
            (w / self.width * 2.0) as f32,
            (-h / self.height * 2.0) as f32,
        ]
    }

    /// A logical rectangle in pixels.
    fn px(&self, r: Rect) -> [f32; 4] {
        let s = self.scale;
        [(r.x * s) as f32, (r.y * s) as f32, (r.w * s) as f32, (r.h * s) as f32]
    }

    /// Layer surfaces and their popups, bottom to top.
    fn layers(&mut self, state: &State, layers: &[smithay::wayland::shell::wlr_layer::Layer]) {
        for (layer, origin) in state.layers_on(layers) {
            let origin = origin.to_f64();
            let surface = layer.wl_surface();
            self.draw_tree(surface, origin);
            for (popup, offset) in PopupManager::popups_for_surface(surface) {
                self.draw_tree(popup.wl_surface(), origin + (offset - popup.geometry().loc).to_f64());
            }
        }
    }

    /// A live window where its animation has it, offset and faded by a
    /// workspace switch.
    fn window(&mut self, settings: &Settings, window: &smithay::desktop::Window, offset: [f64; 2], ws_alpha: f64, fullscreen: bool) {
        let Some(toplevel) = window.toplevel() else { return };
        let Some((rect, goal, alpha, border)) =
            anim::with_anim(window, |a| (*a.rect.value(), *a.rect.goal(), *a.alpha.value(), a.border.value().clone()))
        else {
            return;
        };
        let rect = Rect { x: rect.x + offset[0], y: rect.y + offset[1], ..rect };
        let surface = toplevel.wl_surface();
        let geo = window.geometry().loc;
        let origin: Point<f64, Logical> = (goal.x - geo.x as f64, goal.y - geo.y as f64).into();
        let clipped = crate::wm::clip_of(window).is_some();
        self.decorated(
            settings,
            rect,
            goal,
            alpha * ws_alpha,
            !fullscreen,
            clipped,
            &border,
            |f| f.draw_tree(surface, origin),
            |f| {
                for (popup, offset) in PopupManager::popups_for_surface(surface) {
                    let at = (goal.x + (offset.x - popup.geometry().loc.x) as f64, goal.y + (offset.y - popup.geometry().loc.y) as f64);
                    f.draw_tree(popup.wl_surface(), at.into());
                }
            },
        );
    }

    /// A window's border and content in `rect` (logical, on screen), its
    /// content laid out for `goal`: rounded, cut to the rectangle when it's a
    /// tile (`clipped`), its popups after, uncut.
    #[allow(clippy::too_many_arguments)]
    fn decorated(
        &mut self,
        settings: &Settings,
        rect: Rect,
        goal: Rect,
        alpha: f64,
        decorated: bool,
        clipped: bool,
        border: &Gradient,
        content: impl FnOnce(&mut Self),
        popups: impl FnOnce(&mut Self),
    ) {
        if alpha <= 0.001 || rect.w <= 0.0 || rect.h <= 0.0 || goal.w <= 0.0 || goal.h <= 0.0 {
            return;
        }
        self.alpha = alpha.min(1.0) as f32;
        // Border: a rounded ring just outside the window, in the space the
        // layout reserved for it.
        let b = settings.border_size.max(0) as f64;
        if decorated && b > 0.0 {
            let outer = Rect { x: rect.x - b, y: rect.y - b, w: rect.w + 2.0 * b, h: rect.h + 2.0 * b };
            self.border(outer, settings.rounding as f64 + b, b, border);
        }
        self.xform = Xform { base: (rect.x, rect.y), anchor: (goal.x, goal.y), scale: (rect.w / goal.w, rect.h / goal.h) };
        self.round = (decorated && settings.rounding > 0).then_some((rect, settings.rounding as f64));
        self.clip(clipped.then_some(rect));
        content(self);
        self.clip(None);
        self.round = None;
        popups(self);
        self.xform = Xform::IDENTITY;
        self.alpha = 1.0;
    }

    /// A rounded border ring filling `outer` (logical), `width` thick.
    fn border(&mut self, outer: Rect, radius: f64, width: f64, gradient: &Gradient) {
        let mut quad = Quad::empty();
        let r = self.px(outer);
        quad.dst = self.ndc(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64);
        quad.rect = r;
        quad.radius = (radius.min(outer.w / 2.0).min(outer.h / 2.0) * self.scale) as f32;
        quad.alpha = self.alpha;
        quad.border = (width * self.scale) as f32;
        quad.angle = gradient.angle.to_radians();
        quad.ncolors = gradient.colors.len().clamp(1, 8) as u32;
        for (slot, c) in quad.colors.iter_mut().zip(&gradient.colors) {
            *slot = *c;
        }
        let bytes = NonNull::from(&quad).cast::<c_void>();
        let len = std::mem::size_of::<Quad>();
        // SAFETY: `quad` outlives the calls, which copy it.
        unsafe {
            self.encoder.setRenderPipelineState(&self.renderer.border_pipeline);
            self.encoder.setVertexBytes_length_atIndex(bytes, len, 0);
            self.encoder.setFragmentBytes_length_atIndex(bytes, len, 0);
            self.encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
            self.encoder.setRenderPipelineState(&self.renderer.pipeline);
        }
    }

    /// Restrict drawing to a logical rectangle, or lift the restriction.
    fn clip(&mut self, rect: Option<Rect>) {
        let (w, h) = (self.width as usize, self.height as usize);
        let scissor = match rect {
            None => MTLScissorRect { x: 0, y: 0, width: w, height: h },
            Some(r) => {
                // Logical to pixels, clamped to the drawable (Metal requires it).
                let px = |v: f64, max: usize| ((v * self.scale).round().max(0.0) as usize).min(max);
                let (x0, y0, x1, y1) = (px(r.x, w), px(r.y, h), px(r.x + r.w, w), px(r.y + r.h, h));
                MTLScissorRect { x: x0, y: y0, width: x1.saturating_sub(x0), height: y1.saturating_sub(y0) }
            }
        };
        self.encoder.setScissorRect(scissor);
    }

    /// A surface and its subsurfaces, parents first (Smithay's traversal, as
    /// in its `render_elements_from_surface_tree`).
    fn draw_tree(&mut self, surface: &WlSurface, origin: Point<f64, Logical>) {
        with_surface_tree_downward(
            surface,
            origin,
            |_, states, loc| {
                let view = states.data_map.get::<RendererSurfaceStateUserData>().and_then(|d| d.lock().unwrap().view());
                match view {
                    Some(view) => TraversalAction::DoChildren(*loc + view.offset.to_f64()),
                    None => TraversalAction::SkipChildren,
                }
            },
            |_, states, loc| {
                let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else { return };
                let (view, buffer_size) = {
                    let d = data.lock().unwrap();
                    (d.view(), d.buffer_size())
                };
                let (Some(view), Some(buffer_size)) = (view, buffer_size) else { return };
                let Some((texture, opaque)) = self.renderer.texture_for(states) else { return };
                let pos = *loc + view.offset.to_f64();
                self.quad(&texture, opaque, pos, view.dst.to_f64(), view.src, buffer_size.to_f64());
            },
            |_, _, _| true,
        );
    }

    fn quad(
        &mut self,
        texture: &ProtocolObject<dyn MTLTexture>,
        opaque: bool,
        pos: Point<f64, Logical>,
        size: smithay::utils::Size<f64, Logical>,
        src: smithay::utils::Rectangle<f64, Logical>,
        buffer: smithay::utils::Size<f64, Logical>,
    ) {
        if size.w <= 0.0 || size.h <= 0.0 || buffer.w <= 0.0 || buffer.h <= 0.0 {
            return;
        }
        let pos = self.xform.point(pos);
        let (x, y) = (pos.x * self.scale, pos.y * self.scale);
        let (w, h) = (size.w * self.xform.scale.0 * self.scale, size.h * self.xform.scale.1 * self.scale);
        let mut quad = Quad::empty();
        quad.dst = self.ndc(x, y, w, h);
        quad.src = [
            (src.loc.x / buffer.w) as f32,
            (src.loc.y / buffer.h) as f32,
            (src.size.w / buffer.w) as f32,
            (src.size.h / buffer.h) as f32,
        ];
        quad.opaque = opaque as u32;
        quad.alpha = self.alpha;
        if let Some((rect, radius)) = self.round {
            quad.rect = self.px(rect);
            quad.radius = (radius.min(rect.w / 2.0).min(rect.h / 2.0) * self.scale) as f32;
        }
        let bytes = NonNull::from(&quad).cast::<c_void>();
        let len = std::mem::size_of::<Quad>();
        // SAFETY: `quad` outlives the calls, which copy it.
        unsafe {
            self.encoder.setVertexBytes_length_atIndex(bytes, len, 0);
            self.encoder.setFragmentBytes_length_atIndex(bytes, len, 0);
            self.encoder.setFragmentTexture_atIndex(Some(texture), 0);
            self.encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        }
    }
}
