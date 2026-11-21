//! Metal renderer: each client surface's shm buffer becomes a texture, drawn
//! as a quad into the window's `CAMetalLayer`.
//!
//! M1 uploads a surface's whole buffer whenever it has a new commit and
//! redraws the whole frame; damage tracking comes later.

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
    utils::{Logical, Point},
    wayland::{
        compositor::{SurfaceData, TraversalAction, with_surface_tree_downward},
        shm::with_buffer_contents,
    },
};

use crate::state::State;

const SHADERS: &str = r#"
#include <metal_stdlib>
using namespace metal;

struct Quad {
    float4 dst;     // x, y, w, h in normalized device coordinates
    float4 src;     // u, v, w, h in texture coordinates
    float alpha;
    uint opaque;    // XRGB: ignore the alpha channel
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

fragment float4 quad_fragment(VOut in [[stage_in]], constant Quad &q [[buffer(0)]],
                              texture2d<float> tex [[texture(0)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float4 c = tex.sample(s, in.uv);
    if (q.opaque != 0) { c.a = 1.0; }
    return c * q.alpha;
}
"#;

/// Matches `struct Quad` in the shader (Metal pads it to 16-byte alignment).
#[repr(C)]
struct Quad {
    dst: [f32; 4],
    src: [f32; 4],
    alpha: f32,
    opaque: u32,
    _pad: [u32; 2],
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
        let vertex = library.newFunctionWithName(&NSString::from_str("quad_vertex")).expect("quad_vertex");
        let fragment = library.newFunctionWithName(&NSString::from_str("quad_fragment")).expect("quad_fragment");

        let desc = MTLRenderPipelineDescriptor::new();
        desc.setVertexFunction(Some(&vertex));
        desc.setFragmentFunction(Some(&fragment));
        // SAFETY: index 0 always exists.
        let attachment = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
        attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // Wayland buffers are premultiplied.
        attachment.setBlendingEnabled(true);
        attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
        attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
        attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
        let pipeline = device
            .newRenderPipelineStateWithDescriptor_error(&desc)
            .unwrap_or_else(|err| panic!("render pipeline: {}", err.localizedDescription()));

        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        layer.setFramebufferOnly(true);

        Renderer { device, queue, pipeline, layer }
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

        let mut frame = Frame { renderer: self, encoder: &encoder, scale, width: size.w, height: size.h };
        for window in state.space.elements() {
            let (Some(loc), Some(toplevel)) = (state.space.element_location(window), window.toplevel()) else {
                continue;
            };
            let surface = toplevel.wl_surface();
            // A tiled window draws only inside its tile; its popups anywhere.
            frame.clip(crate::wm::clip_of(window));
            frame.draw_tree(surface, (loc - window.geometry().loc).to_f64());
            frame.clip(None);
            for (popup, offset) in PopupManager::popups_for_surface(surface) {
                frame.draw_tree(popup.wl_surface(), (loc + offset - popup.geometry().loc).to_f64());
            }
        }

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
}

impl Frame<'_> {
    /// Restrict drawing to a logical rectangle, or lift the restriction.
    fn clip(&mut self, rect: Option<smithay::utils::Rectangle<i32, Logical>>) {
        let (w, h) = (self.width as usize, self.height as usize);
        let scissor = match rect {
            None => MTLScissorRect { x: 0, y: 0, width: w, height: h },
            Some(r) => {
                // Logical to pixels, clamped to the drawable (Metal requires it).
                let x0 = ((r.loc.x as f64 * self.scale).round().max(0.0) as usize).min(w);
                let y0 = ((r.loc.y as f64 * self.scale).round().max(0.0) as usize).min(h);
                let x1 = (((r.loc.x + r.size.w) as f64 * self.scale).round().max(0.0) as usize).min(w);
                let y1 = (((r.loc.y + r.size.h) as f64 * self.scale).round().max(0.0) as usize).min(h);
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
        let (x, y) = (pos.x * self.scale, pos.y * self.scale);
        let (w, h) = (size.w * self.scale, size.h * self.scale);
        let quad = Quad {
            dst: [
                (x / self.width * 2.0 - 1.0) as f32,
                (1.0 - y / self.height * 2.0) as f32,
                (w / self.width * 2.0) as f32,
                (-h / self.height * 2.0) as f32,
            ],
            src: [
                (src.loc.x / buffer.w) as f32,
                (src.loc.y / buffer.h) as f32,
                (src.size.w / buffer.w) as f32,
                (src.size.h / buffer.h) as f32,
            ],
            alpha: 1.0,
            opaque: opaque as u32,
            _pad: [0; 2],
        };
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
