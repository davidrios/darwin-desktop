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

// ---- shadows (Hyprland's shadow.glsl, BSD 3-Clause, see tests/HYPRLAND-LICENSE) ----

struct Shadow {
    float4 dst;        // normalized device coordinates
    float4 rect;       // the shadow's box in pixels
    float4 window;     // the window it's cut out under, in pixels (w <= 0: none)
    float4 color;      // premultiplied, times the window's alpha
    float radius;      // corner radius (window rounding + border), pixels
    float range;       // falloff distance, pixels
    float power;       // falloff exponent
    float window_radius;
};

vertex VOut shadow_vertex(uint vid [[vertex_id]], constant Shadow &s [[buffer(0)]]) {
    float2 corner = float2(vid & 1, vid >> 1);
    VOut o;
    o.pos = float4(s.dst.xy + corner * s.dst.zw, 0.0, 1.0);
    o.uv = corner;
    return o;
}

static float shadow_falloff(float d, float radius, float range, float power) {
    if (d > radius) { return 0.0; }
    if (d > radius - range) { return pow((range - (d - radius + range)) / range, power); }
    return 1.0;
}

fragment float4 shadow_fragment(VOut in [[stage_in]], constant Shadow &s [[buffer(0)]]) {
    float2 full = s.rect.zw;
    float2 p = in.pos.xy - s.rect.xy;
    float radius = s.range + s.radius;
    float2 tl = float2(radius);
    float2 br = full - radius;
    float a = 1.0;
    bool corner = true;
    if (p.x < tl.x && p.y < tl.y) {
        a = shadow_falloff(length(p - tl), radius, s.range, s.power);
    } else if (p.x < tl.x && p.y > br.y) {
        a = shadow_falloff(length(p - float2(tl.x, br.y)), radius, s.range, s.power);
    } else if (p.x > br.x && p.y < tl.y) {
        a = shadow_falloff(length(p - float2(br.x, tl.y)), radius, s.range, s.power);
    } else if (p.x > br.x && p.y > br.y) {
        a = shadow_falloff(length(p - br), radius, s.range, s.power);
    } else {
        corner = false;
    }
    if (!corner && s.range > 0.0) {
        float m = min(min(p.y, full.y - p.y), min(p.x, full.x - p.x));
        if (m < s.range) { a = pow(m / s.range, s.power); }
    }
    if (s.window.z > 0.0) {
        // Not under the window itself (Hyprland's ignore_window).
        a *= saturate(0.5 + rounded_rect(in.pos.xy, s.window, s.window_radius));
    }
    return s.color * a;
}

// ---- blur: Hyprland's dual Kawase passes (blurprepare, blur1, blur2,
// blurFinish; BSD 3-Clause, see tests/HYPRLAND-LICENSE) ----

struct Blur {
    float2 halfpixel;
    float radius;
    float passes;
    float vibrancy;
    float vibrancy_darkness;
    float contrast;
    float brightness;
    float noise;
    float3 _pad;
};

static float3 gain(float3 src, float k) {
    float3 x = saturate(src);
    float3 t = step(0.5, x);
    float3 y = mix(x, 1.0 - x, t);
    float3 a = 0.5 * pow(2.0 * y, float3(k));
    return mix(a, 1.0 - a, t);
}

fragment float4 blur_prepare(VOut in [[stage_in]], texture2d<float> tex [[texture(0)]], constant Blur &b [[buffer(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float4 c = tex.sample(s, in.uv);
    if (c.a <= 0.0) { return float4(0.0); }
    if (b.contrast != 1.0) { c.rgb = gain(c.rgb / c.a, b.contrast) * c.a; }
    c.rgb *= max(1.0, b.brightness);
    return c;
}

static float double_circle_sigmoid(float x, float a) {
    a = saturate(a);
    if (x <= a) { return a - sqrt(a * a - x * x); }
    return a + sqrt(pow(1.0 - a, 2.0) - pow(x - 1.0, 2.0));
}

static float3 rgb2hsl(float3 col) {
    float minc = min(col.r, min(col.g, col.b));
    float maxc = max(col.r, max(col.g, col.b));
    float delta = maxc - minc;
    float lum = (minc + maxc) * 0.5;
    float sat = 0.0;
    float hue = 0.0;
    if (lum > 0.0 && lum < 1.0) {
        float mul = (lum < 0.5) ? lum : (1.0 - lum);
        sat = delta / (mul * 2.0);
    }
    if (delta > 0.0) {
        float3 maxv = float3(maxc);
        float3 masks = float3(maxv == col) * float3(maxv != float3(col.g, col.b, col.r));
        float3 adds = float3(0.0, 2.0, 4.0) + float3(col.g - col.b, col.b - col.r, col.r - col.g) / delta;
        hue += dot(adds, masks);
        hue /= 6.0;
        if (hue < 0.0) { hue += 1.0; }
    }
    return float3(hue, sat, lum);
}

static float3 hsl2rgb(float3 col) {
    const float onethird = 1.0 / 3.0;
    const float twothird = 2.0 / 3.0;
    const float rcpsixth = 6.0;
    float hue = col.x, sat = col.y, lum = col.z;
    float3 xt = float3(0.0);
    if (hue < onethird) {
        xt = float3(rcpsixth * (onethird - hue), rcpsixth * hue, 0.0);
    } else if (hue < twothird) {
        xt = float3(0.0, rcpsixth * (twothird - hue), rcpsixth * (hue - onethird));
    } else {
        xt = float3(rcpsixth * (hue - twothird), 0.0, rcpsixth * (1.0 - hue));
    }
    xt = min(xt, 1.0);
    float3 ct = (2.0 * sat * xt) + (1.0 - sat);
    return lum >= 0.5 ? ((1.0 - lum) * ct) + (2.0 * lum - 1.0) : lum * ct;
}

// Downsample (blur1): `in.uv` samples the bigger source; vibrancy boosts saturation.
fragment float4 blur_down(VOut in [[stage_in]], texture2d<float> tex [[texture(0)]], constant Blur &b [[buffer(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float2 uv = in.uv;
    float2 h = b.halfpixel * b.radius;
    float4 sum = tex.sample(s, uv) * 4.0;
    sum += tex.sample(s, uv - h);
    sum += tex.sample(s, uv + h);
    sum += tex.sample(s, uv + float2(h.x, -h.y));
    sum += tex.sample(s, uv - float2(h.x, -h.y));
    float4 color = sum / 8.0;
    if (b.vibrancy == 0.0 || color.a <= 0.0) { return color; }
    color.rgb /= color.a;
    const float Pr = 0.299, Pg = 0.587, Pb = 0.114;
    const float ka = 0.93, kb = 0.11, kc = 0.66;
    float darkness = 1.0 - b.vibrancy_darkness;
    float3 hsl = rgb2hsl(color.rgb);
    float perceived = double_circle_sigmoid(sqrt(color.r * color.r * Pr + color.g * color.g * Pg + color.b * color.b * Pb), 0.8 * darkness);
    float b1 = kb * darkness;
    float boost = hsl.y > 0.0
        ? smoothstep(b1 - kc * 0.5, b1 + kc * 0.5, 1.0 - (pow(1.0 - hsl.y * cos(ka), 2.0) + pow(1.0 - perceived * sin(ka), 2.0)))
        : 0.0;
    float saturation = saturate(hsl.y + (boost * b.vibrancy) / b.passes);
    return float4(hsl2rgb(float3(hsl.x, saturation, hsl.z)) * color.a, color.a);
}

// Upsample (blur2): `in.uv` samples the smaller source.
fragment float4 blur_up(VOut in [[stage_in]], texture2d<float> tex [[texture(0)]], constant Blur &b [[buffer(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float2 uv = in.uv;
    float2 h = b.halfpixel * b.radius;
    float4 sum = tex.sample(s, uv + float2(-h.x * 2.0, 0.0));
    sum += tex.sample(s, uv + float2(-h.x, h.y)) * 2.0;
    sum += tex.sample(s, uv + float2(0.0, h.y * 2.0));
    sum += tex.sample(s, uv + float2(h.x, h.y)) * 2.0;
    sum += tex.sample(s, uv + float2(h.x * 2.0, 0.0));
    sum += tex.sample(s, uv + float2(h.x, -h.y)) * 2.0;
    sum += tex.sample(s, uv + float2(0.0, -h.y * 2.0));
    sum += tex.sample(s, uv + float2(-h.x, -h.y)) * 2.0;
    return sum / 12.0;
}

static float hash(float2 p) {
    float3 p3 = fract(float3(p.xyx) * 1689.1984);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}

fragment float4 blur_finish(VOut in [[stage_in]], texture2d<float> tex [[texture(0)]], constant Blur &b [[buffer(1)]]) {
    constexpr sampler s(filter::linear, address::clamp_to_edge);
    float4 c = tex.sample(s, in.uv);
    c.rgb += (hash(in.uv) - 0.5) * b.noise * c.a;
    c.rgb *= min(1.0, b.brightness);
    return c;
}
"#;

/// Matches `struct Shadow` in the shader.
#[repr(C)]
#[derive(Clone, Copy)]
struct ShadowQuad {
    dst: [f32; 4],
    rect: [f32; 4],
    window: [f32; 4],
    color: [f32; 4],
    radius: f32,
    range: f32,
    power: f32,
    window_radius: f32,
}

const _: () = assert!(std::mem::size_of::<ShadowQuad>() == 80);

/// Matches `struct Blur` in the shader (float3 is 16 bytes, 16-aligned).
#[repr(C)]
#[derive(Clone, Copy)]
struct BlurParams {
    halfpixel: [f32; 2],
    radius: f32,
    passes: f32,
    vibrancy: f32,
    vibrancy_darkness: f32,
    contrast: f32,
    brightness: f32,
    noise: f32,
    _pad: [f32; 7],
}

const _: () = assert!(std::mem::size_of::<BlurParams>() == 64);

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
    shadow_pipeline: Retained<ProtocolObject<dyn MTLRenderPipelineState>>,
    /// prepare, down, up, finish.
    blur_pipelines: [Retained<ProtocolObject<dyn MTLRenderPipelineState>>; 4],
    blur_targets: RefCell<Option<BlurTargets>>,
    /// Group bar titles, drawn once per title, size and colour.
    titles: RefCell<std::collections::HashMap<TitleKey, Texture>>,
    pub layer: Retained<CAMetalLayer>,
}

/// Textures for blurring, for one drawable size: the levels of the dual
/// Kawase chain (level i is 1/2^i the size) and two results (the background
/// blur, kept for the frame, and the latest live blur).
struct BlurTargets {
    size: (usize, usize),
    levels: Vec<Texture>,
    results: [Texture; 2],
}

/// Which blurred backdrop a translucent window shows through.
#[derive(Clone, Copy, PartialEq)]
enum Backdrop {
    None,
    /// The wallpaper and bottom layers, blurred once a frame (tiled windows).
    Background,
    /// Everything drawn so far, blurred now (floating windows over tiles).
    Live,
}

/// How a window is decorated.
struct Look<'a> {
    /// Border and shadow drawn.
    decorated: bool,
    clipped: bool,
    focused: bool,
    border: &'a Gradient,
    backdrop: Backdrop,
    /// Corner radius and border width (logical): the config's, or a rule's.
    rounding: f64,
    border_size: f64,
    shadow: bool,
    /// A group's tab bar above the window.
    group: Option<crate::groups::GroupLook>,
}

/// A title drawn into a texture, by everything that changes its pixels.
#[derive(Clone, PartialEq, Eq, Hash)]
struct TitleKey {
    text: String,
    width: usize,
    height: usize,
    font_px: u32,
    color: [u8; 4],
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
        let pipeline_with = |vertex: &str, fragment: &str, blend: bool| {
            let desc = MTLRenderPipelineDescriptor::new();
            desc.setVertexFunction(Some(&function(vertex)));
            desc.setFragmentFunction(Some(&function(fragment)));
            // SAFETY: index 0 always exists.
            let attachment = unsafe { desc.colorAttachments().objectAtIndexedSubscript(0) };
            attachment.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
            // Wayland buffers and our colours are premultiplied.
            attachment.setBlendingEnabled(blend);
            attachment.setSourceRGBBlendFactor(MTLBlendFactor::One);
            attachment.setSourceAlphaBlendFactor(MTLBlendFactor::One);
            attachment.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            attachment.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
            device
                .newRenderPipelineStateWithDescriptor_error(&desc)
                .unwrap_or_else(|err| panic!("render pipeline {fragment}: {}", err.localizedDescription()))
        };
        let pipeline = pipeline_with("quad_vertex", "quad_fragment", true);
        let border_pipeline = pipeline_with("quad_vertex", "border_fragment", true);
        let shadow_pipeline = pipeline_with("shadow_vertex", "shadow_fragment", true);
        let blur_pipelines = ["blur_prepare", "blur_down", "blur_up", "blur_finish"]
            .map(|f| pipeline_with("quad_vertex", f, false));

        let layer = CAMetalLayer::new();
        layer.setDevice(Some(&device));
        layer.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
        // Blur reads back what's been drawn.
        layer.setFramebufferOnly(false);

        Renderer {
            device,
            queue,
            pipeline,
            border_pipeline,
            shadow_pipeline,
            blur_pipelines,
            blur_targets: RefCell::new(None),
            titles: RefCell::new(Default::default()),
            layer,
        }
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
            encoder,
            commands: &commands,
            target: &target,
            scale,
            width: size.w,
            height: size.h,
            logical: (state.output_size.w as f64, state.output_size.h as f64),
            round: None,
            xform: Xform::IDENTITY,
            alpha: 1.0,
            background_blur: None,
        };
        use smithay::wayland::shell::wlr_layer::Layer;
        frame.layers(state, &[Layer::Background, Layer::Bottom]);
        // The background blur, once, if anything on screen shows through to it.
        if blur_on(settings) && frame_needs_blur(state) {
            frame.background_blur = frame.blur(settings, 0);
        }
        let transition = state.wm.transition.as_ref();
        // A workspace switch: the workspace being left, sliding or fading out.
        if let Some(tr) = transition {
            if let Some(ws) = state.wm.workspaces.get(&tr.from) {
                let mut windows = ws.shown();
                if let Some(fs) = &ws.fullscreen {
                    windows.retain(|w| w != fs);
                    windows.push(fs.clone());
                }
                for window in &windows {
                    let fullscreen = ws.fullscreen.as_ref() == Some(window);
                    let floating = ws.floating.contains(window);
                    let focused = false;
                    frame.window(settings, window, *tr.out_offset.value(), *tr.out_alpha.value(), fullscreen, floating, focused, &state.rule_effects(window), state.group_look(window));
                }
            }
        }
        let (offset, alpha) = transition.map(|t| (*t.in_offset.value(), *t.in_alpha.value())).unwrap_or(([0.0, 0.0], 1.0));
        let focused = state.focused_window();
        let special = state.wm.special;
        let in_special =
            |w: &smithay::desktop::Window| special.is_some_and(|s| state.wm.workspaces.get(&s).is_some_and(|ws| ws.contains(w)));
        for window in state.space.elements().filter(|w| !in_special(w)) {
            let (fullscreen, floating) = (state.is_fullscreen(window), state.is_floating_here(window));
            let is_focused = focused.as_ref() == Some(window);
            frame.window(settings, window, offset, alpha, fullscreen, floating, is_focused, &state.rule_effects(window), state.group_look(window));
        }
        // A special workspace over the active one, which dims under it.
        let special_anim = state.wm.special_anim.as_ref();
        let hiding = special_anim.filter(|a| !a.showing);
        if special.is_some() || hiding.is_some() {
            let (s_alpha, s_offset) = special_anim.map(|a| (*a.alpha.value(), *a.offset.value())).unwrap_or((1.0, [0.0, 0.0]));
            let dim = (settings.dim_special * s_alpha) as f32;
            if dim > 0.0 {
                let (w, h) = frame.logical;
                frame.fill(Rect::new(0.0, 0.0, w, h), [0.0, 0.0, 0.0, dim]);
            }
            if special.is_some() {
                for window in state.space.elements().filter(|w| in_special(w)) {
                    let (fullscreen, floating) = (state.is_fullscreen(window), state.is_floating_here(window));
                    let is_focused = focused.as_ref() == Some(window);
                    frame.window(settings, window, s_offset, s_alpha, fullscreen, floating, is_focused, &state.rule_effects(window), state.group_look(window));
                }
            } else if let Some(ws) = hiding.and_then(|a| state.wm.workspaces.get(&a.id)) {
                for window in ws.shown() {
                    let fullscreen = ws.fullscreen.as_ref() == Some(&window);
                    let floating = ws.floating.contains(&window);
                    frame.window(settings, &window, s_offset, s_alpha, fullscreen, floating, false, &state.rule_effects(&window), state.group_look(&window));
                }
            }
        }
        for closing in &state.closing {
            let translucent = closing.pieces.iter().any(|p| !p.opaque);
            let look = Look {
                decorated: closing.decorated,
                clipped: closing.clipped,
                focused: false,
                border: &closing.border,
                backdrop: if translucent { Backdrop::Background } else { Backdrop::None },
                rounding: if closing.decorated { settings.rounding.max(0) as f64 } else { 0.0 },
                border_size: settings.border_size.max(0) as f64,
                shadow: true,
                group: None,
            };
            frame.decorated(
                settings,
                *closing.rect.value(),
                closing.anchor,
                *closing.alpha.value(),
                &look,
                |f| {
                    for p in &closing.pieces {
                        f.quad(&p.texture, p.opaque, p.pos, p.size, p.src, p.buffer);
                    }
                },
                |_| {},
            );
        }
        frame.layers(state, state.upper_layers());

        frame.encoder.endEncoding();
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

    /// A title's texture, drawn with Core Text the first time it's needed.
    fn title_texture(&self, key: &TitleKey, font: Option<&str>) -> Option<Texture> {
        if let Some(t) = self.titles.borrow().get(key) {
            return Some(t.clone());
        }
        let color = key.color.map(|c| c as f32 / 255.0);
        let bitmap = crate::text::render_line(&key.text, font, key.font_px as f64, key.width, key.height, color)?;
        let texture = self.new_texture(bitmap.width, bitmap.height)?;
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize { width: bitmap.width, height: bitmap.height, depth: 1 },
        };
        // SAFETY: the bitmap holds width*height BGRA pixels, rows width*4 apart.
        unsafe {
            let bytes = NonNull::new_unchecked(bitmap.pixels.as_ptr() as *mut c_void);
            texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, bitmap.width * 4);
        }
        let mut titles = self.titles.borrow_mut();
        // Titles change (a shell prompt, a page title): don't keep them all.
        if titles.len() > 256 {
            titles.clear();
        }
        titles.insert(key.clone(), texture.clone());
        Some(texture)
    }

    /// The blur textures for a drawable size, made (again) when it changes.
    fn blur_targets(&self, size: (usize, usize)) -> Option<std::cell::Ref<'_, BlurTargets>> {
        let stale = self.blur_targets.borrow().as_ref().is_none_or(|t| t.size != size);
        if stale {
            let make = |w: usize, h: usize| {
                // SAFETY: plain descriptor construction.
                let desc = unsafe {
                    MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                        MTLPixelFormat::BGRA8Unorm,
                        w.max(1),
                        h.max(1),
                        false,
                    )
                };
                desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);
                desc.setStorageMode(objc2_metal::MTLStorageMode::Private);
                self.device.newTextureWithDescriptor(&desc)
            };
            let levels = (0..=8).map(|i| make(size.0 >> i, size.1 >> i)).collect::<Option<Vec<_>>>()?;
            let results = [make(size.0, size.1)?, make(size.0, size.1)?];
            *self.blur_targets.borrow_mut() = Some(BlurTargets { size, levels, results });
        }
        std::cell::Ref::filter_map(self.blur_targets.borrow(), Option::as_ref).ok()
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
    /// The pass drawing into the drawable; ended and started again around blurs.
    encoder: Retained<ProtocolObject<dyn MTLRenderCommandEncoder>>,
    commands: &'a ProtocolObject<dyn MTLCommandBuffer>,
    target: &'a ProtocolObject<dyn MTLTexture>,
    scale: f64,
    width: f64,
    height: f64,
    /// The output in logical points.
    logical: (f64, f64),
    /// The wallpaper and bottom layers blurred, if anything needs it.
    background_blur: Option<Texture>,
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
    #[allow(clippy::too_many_arguments)]
    fn window(
        &mut self,
        settings: &Settings,
        window: &smithay::desktop::Window,
        offset: [f64; 2],
        ws_alpha: f64,
        fullscreen: bool,
        floating: bool,
        focused: bool,
        rules: &crate::rules::Effects,
        group: Option<crate::groups::GroupLook>,
    ) {
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
        let backdrop = if !blur_on(settings) || rules.no_blur || surface_opaque(surface, window.geometry()) {
            Backdrop::None
        } else if floating && !settings.blur.xray {
            Backdrop::Live
        } else {
            Backdrop::Background
        };
        let look = Look {
            decorated: !fullscreen && rules.decorate != Some(false),
            clipped,
            focused,
            border: &border,
            backdrop,
            rounding: if fullscreen { 0.0 } else { rules.rounding.unwrap_or(settings.rounding as f64).max(0.0) },
            border_size: rules.border_size.unwrap_or(settings.border_size as f64).max(0.0),
            shadow: !rules.no_shadow,
            group,
        };
        let opacity = rules.opacity.map(|(active, inactive)| if focused { active } else { inactive }).unwrap_or(1.0);
        self.decorated(
            settings,
            rect,
            goal,
            alpha * ws_alpha * opacity.clamp(0.0, 1.0),
            &look,
            |f| f.draw_tree(surface, origin),
            |f| {
                for (popup, offset) in PopupManager::popups_for_surface(surface) {
                    let at = (goal.x + (offset.x - popup.geometry().loc.x) as f64, goal.y + (offset.y - popup.geometry().loc.y) as f64);
                    f.draw_tree(popup.wl_surface(), at.into());
                }
            },
        );
    }

    /// A window in `rect` (logical, on screen), its content laid out for
    /// `goal`: shadow, blurred backdrop, border, then the content, rounded and
    /// cut to the rectangle when it's a tile; its popups after, uncut.
    #[allow(clippy::too_many_arguments)]
    fn decorated(
        &mut self,
        settings: &Settings,
        rect: Rect,
        goal: Rect,
        alpha: f64,
        look: &Look,
        content: impl FnOnce(&mut Self),
        popups: impl FnOnce(&mut Self),
    ) {
        if alpha <= 0.001 || rect.w <= 0.0 || rect.h <= 0.0 || goal.w <= 0.0 || goal.h <= 0.0 {
            return;
        }
        self.alpha = alpha.min(1.0) as f32;
        let b = look.border_size;
        let rounding = look.rounding;
        let shadow = &settings.shadow;
        if look.decorated && look.shadow && shadow.enabled && shadow.range > 0.0 {
            let color = if look.focused { shadow.color } else { shadow.color_inactive.unwrap_or(shadow.color) };
            self.shadow(rect, b, rounding, shadow, color);
        }
        // What's behind, blurred, under a window that isn't opaque.
        let backdrop = match look.backdrop {
            Backdrop::None => None,
            Backdrop::Background => self.background_blur.clone(),
            Backdrop::Live => self.blur(settings, 1),
        };
        if let Some(texture) = backdrop {
            self.backdrop(&texture, rect, rounding);
        }
        // Border: a rounded ring just outside the window, in the space the
        // layout reserved for it.
        if look.decorated && b > 0.0 {
            let outer = Rect { x: rect.x - b, y: rect.y - b, w: rect.w + 2.0 * b, h: rect.h + 2.0 * b };
            self.border(outer, rounding + b, b, look.border);
        }
        if let (true, Some(group)) = (look.decorated, &look.group) {
            self.groupbar(settings, rect, b, group);
        }
        self.xform = Xform { base: (rect.x, rect.y), anchor: (goal.x, goal.y), scale: (rect.w / goal.w, rect.h / goal.h) };
        self.round = (rounding > 0.0).then_some((rect, rounding));
        self.clip(look.clipped.then_some(rect));
        content(self);
        self.clip(None);
        self.round = None;
        popups(self);
        self.xform = Xform::IDENTITY;
        self.alpha = 1.0;
    }

    /// A drop shadow for a window at `rect` with a `border`, as Hyprland's:
    /// the box is the window and its border grown by the range, then scaled
    /// from its centre and offset; it isn't drawn under the window itself.
    fn shadow(&mut self, rect: Rect, border: f64, rounding: f64, cfg: &crate::config::ShadowConfig, color: [f32; 4]) {
        let range = cfg.range;
        let grow = border + range;
        let full = Rect { x: rect.x - grow, y: rect.y - grow, w: rect.w + 2.0 * grow, h: rect.h + 2.0 * grow };
        let (w, h) = (full.w * cfg.scale, full.h * cfg.scale);
        let full = Rect {
            x: full.x + (full.w - w) / 2.0 + cfg.offset.0,
            y: full.y + (full.h - h) / 2.0 + cfg.offset.1,
            w,
            h,
        };
        if full.w < 1.0 || full.h < 1.0 {
            return;
        }
        let s = self.scale;
        let radius = if rounding > 0.0 { rounding + border } else { 0.0 };
        let r = self.px(full);
        let quad = ShadowQuad {
            dst: self.ndc(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64),
            rect: r,
            window: self.px(rect),
            color: color.map(|c| c * self.alpha),
            radius: (radius * s) as f32,
            // Sharp: no falloff, a solid rounded box.
            range: if cfg.sharp { 0.0 } else { (range * s) as f32 },
            power: cfg.power as f32,
            window_radius: (rounding.min(rect.w / 2.0).min(rect.h / 2.0) * s) as f32,
        };
        let bytes = NonNull::from(&quad).cast::<c_void>();
        let len = std::mem::size_of::<ShadowQuad>();
        // SAFETY: `quad` outlives the calls, which copy it.
        unsafe {
            self.encoder.setRenderPipelineState(&self.renderer.shadow_pipeline);
            self.encoder.setVertexBytes_length_atIndex(bytes, len, 0);
            self.encoder.setFragmentBytes_length_atIndex(bytes, len, 0);
            self.encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
            self.encoder.setRenderPipelineState(&self.renderer.pipeline);
        }
    }

    /// A group's tab bar above a window at `rect` with border `b`, as
    /// Hyprland's groupbar: per member, an indicator line (the active
    /// colour for the focused one) under its title.
    fn groupbar(&mut self, settings: &Settings, rect: Rect, b: f64, group: &crate::groups::GroupLook) {
        let bar = &settings.group.bar;
        let h = bar.reserved();
        let bx = Rect { x: rect.x - b, y: rect.y - b - h, w: rect.w + 2.0 * b, h };
        let n = group.titles.len().max(1) as f64;
        let tab = (bx.w - bar.gaps_in * (n - 1.0)) / n;
        if tab < 1.0 {
            return;
        }
        let alpha = self.alpha;
        for (i, title) in group.titles.iter().enumerate() {
            let x = bx.x + i as f64 * (tab + bar.gaps_in);
            let active = group.focused == Some(i);
            let (col, text) = match (group.locked, active) {
                (false, true) => (bar.col_active, bar.text_color),
                (false, false) => (bar.col_inactive, bar.text_color_inactive),
                (true, true) => (bar.col_locked_active, bar.text_color_locked_active),
                (true, false) => (bar.col_locked_inactive, bar.text_color_locked_inactive),
            };
            if bar.indicator_height > 0.0 {
                let indicator = Rect { x, y: bx.y + bx.h - bar.indicator_height - bar.gaps_out, w: tab, h: bar.indicator_height };
                self.fill_rounded(indicator, col.map(|c| c * alpha), bar.rounding);
            }
            if bar.render_titles && !title.is_empty() {
                let strip_y = bx.y + bx.h - bar.gaps_out - bar.indicator_height - bar.indicator_gap - bar.height;
                let s = self.scale;
                let text_h = bar.font_size + 4.0;
                let width = ((tab - 2.0 * bar.text_padding) * s).round().max(1.0) as usize;
                let height = (text_h * s).round().max(1.0) as usize;
                let key = TitleKey {
                    text: title.clone(),
                    width,
                    height,
                    font_px: (bar.font_size * s).round() as u32,
                    color: text.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8),
                };
                if let Some(texture) = self.renderer.title_texture(&key, bar.font_family.as_deref()) {
                    let y = strip_y + (bar.height - text_h) / 2.0 - bar.text_offset;
                    let r = self.px(Rect { x: x + bar.text_padding, y, w: width as f64 / s, h: height as f64 / s });
                    let mut quad = Quad::empty();
                    quad.dst = self.ndc(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64);
                    quad.alpha = alpha;
                    let bytes = NonNull::from(&quad).cast::<c_void>();
                    let len = std::mem::size_of::<Quad>();
                    // SAFETY: `quad` outlives the calls, which copy it.
                    unsafe {
                        self.encoder.setVertexBytes_length_atIndex(bytes, len, 0);
                        self.encoder.setFragmentBytes_length_atIndex(bytes, len, 0);
                        self.encoder.setFragmentTexture_atIndex(Some(&texture), 0);
                        self.encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
                    }
                }
            }
        }
    }

    /// A solid rectangle (logical), `color` premultiplied.
    fn fill(&mut self, rect: Rect, color: [f32; 4]) {
        self.fill_rounded(rect, color, 0.0);
    }

    /// A solid rectangle (logical) with rounded corners, `color` premultiplied.
    fn fill_rounded(&mut self, rect: Rect, color: [f32; 4], radius: f64) {
        let r = self.px(rect);
        let quad = ShadowQuad {
            dst: self.ndc(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64),
            rect: r,
            window: [0.0; 4],
            color,
            radius: (radius.min(rect.w / 2.0).min(rect.h / 2.0) * self.scale) as f32,
            range: 0.0,
            power: 1.0,
            window_radius: 0.0,
        };
        let bytes = NonNull::from(&quad).cast::<c_void>();
        let len = std::mem::size_of::<ShadowQuad>();
        // SAFETY: `quad` outlives the calls, which copy it.
        unsafe {
            self.encoder.setRenderPipelineState(&self.renderer.shadow_pipeline);
            self.encoder.setVertexBytes_length_atIndex(bytes, len, 0);
            self.encoder.setFragmentBytes_length_atIndex(bytes, len, 0);
            self.encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
            self.encoder.setRenderPipelineState(&self.renderer.pipeline);
        }
    }

    /// A blurred texture of the whole output, shown in `rect` (rounded).
    fn backdrop(&mut self, texture: &ProtocolObject<dyn MTLTexture>, rect: Rect, rounding: f64) {
        let r = self.px(rect);
        let mut quad = Quad::empty();
        quad.dst = self.ndc(r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64);
        quad.src = [
            (rect.x / self.logical.0) as f32,
            (rect.y / self.logical.1) as f32,
            (rect.w / self.logical.0) as f32,
            (rect.h / self.logical.1) as f32,
        ];
        quad.opaque = 1;
        quad.alpha = self.alpha;
        quad.rect = r;
        quad.radius = (rounding.min(rect.w / 2.0).min(rect.h / 2.0) * self.scale) as f32;
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

    /// Blurs what's been drawn so far into result texture `slot`: ends the
    /// current pass, runs Hyprland's passes (prepare, `passes` downsamples,
    /// as many upsamples, finish) and starts drawing again where it was.
    fn blur(&mut self, settings: &Settings, slot: usize) -> Option<Texture> {
        let cfg = &settings.blur;
        let passes = cfg.passes.clamp(1, 8) as usize;
        let size = (self.target.width(), self.target.height());
        let targets = self.renderer.blur_targets(size)?;
        self.encoder.endEncoding();
        let params = |halfpixel: [f32; 2]| BlurParams {
            halfpixel,
            radius: cfg.size as f32,
            passes: passes as f32,
            vibrancy: cfg.vibrancy as f32,
            vibrancy_darkness: cfg.vibrancy_darkness as f32,
            contrast: cfg.contrast as f32,
            brightness: cfg.brightness as f32,
            noise: cfg.noise as f32,
            _pad: [0.0; 7],
        };
        let inv = |t: &ProtocolObject<dyn MTLTexture>, k: f32| [1.0 / (t.width() as f32 * k), 1.0 / (t.height() as f32 * k)];
        let [prepare, down, up, finish] = &self.renderer.blur_pipelines;
        let mut ok = self.blur_pass(prepare, self.target, &targets.levels[0], params([0.0; 2]));
        for i in 1..=passes {
            ok &= self.blur_pass(down, &targets.levels[i - 1], &targets.levels[i], params(inv(&targets.levels[i - 1], 1.0)));
        }
        for i in (0..passes).rev() {
            ok &= self.blur_pass(up, &targets.levels[i + 1], &targets.levels[i], params(inv(&targets.levels[i + 1], 4.0)));
        }
        ok &= self.blur_pass(finish, &targets.levels[0], &targets.results[slot], params([0.0; 2]));

        // Carry on drawing into the drawable, keeping what's there.
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(self.target));
        color.setLoadAction(MTLLoadAction::Load);
        color.setStoreAction(MTLStoreAction::Store);
        let encoder = self.commands.renderCommandEncoderWithDescriptor(&pass)?;
        encoder.setRenderPipelineState(&self.renderer.pipeline);
        self.encoder = encoder;
        ok.then(|| targets.results[slot].clone())
    }

    /// One full-screen blur pass from `src` into `dst`.
    fn blur_pass(
        &self,
        pipeline: &ProtocolObject<dyn MTLRenderPipelineState>,
        src: &ProtocolObject<dyn MTLTexture>,
        dst: &ProtocolObject<dyn MTLTexture>,
        params: BlurParams,
    ) -> bool {
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(dst));
        color.setLoadAction(MTLLoadAction::DontCare);
        color.setStoreAction(MTLStoreAction::Store);
        let Some(encoder) = self.commands.renderCommandEncoderWithDescriptor(&pass) else { return false };
        let mut quad = Quad::empty();
        quad.dst = [-1.0, 1.0, 2.0, -2.0];
        let qbytes = NonNull::from(&quad).cast::<c_void>();
        let pbytes = NonNull::from(&params).cast::<c_void>();
        // SAFETY: `quad` and `params` outlive the calls, which copy them.
        unsafe {
            encoder.setRenderPipelineState(pipeline);
            encoder.setVertexBytes_length_atIndex(qbytes, std::mem::size_of::<Quad>(), 0);
            encoder.setFragmentBytes_length_atIndex(pbytes, std::mem::size_of::<BlurParams>(), 1);
            encoder.setFragmentTexture_atIndex(Some(src), 0);
            encoder.drawPrimitives_vertexStart_vertexCount(MTLPrimitiveType::TriangleStrip, 0, 4);
        }
        encoder.endEncoding();
        true
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

fn blur_on(settings: &Settings) -> bool {
    settings.blur.enabled && settings.blur.passes > 0 && settings.blur.size > 0.0
}

/// Whether a surface covers its window geometry with opaque pixels: an
/// XRGB buffer, or an opaque region over the whole geometry. Blur is
/// invisible behind those and skipped.
fn surface_opaque(surface: &WlSurface, geometry: Rectangle<i32, Logical>) -> bool {
    smithay::wayland::compositor::with_states(surface, |states| {
        let xrgb = states
            .data_map
            .get::<RefCell<Option<SurfaceTexture>>>()
            .and_then(|c| c.borrow().as_ref().map(|t| t.opaque))
            .unwrap_or(false);
        if xrgb {
            return true;
        }
        let mut attrs = states.cached_state.get::<smithay::wayland::compositor::SurfaceAttributes>();
        let Some(region) = attrs.current().opaque_region.clone() else { return false };
        use smithay::wayland::compositor::RectangleKind;
        region.rects.iter().all(|(kind, _)| matches!(kind, RectangleKind::Add))
            && region.rects.iter().any(|(_, r)| r.contains_rect(geometry))
    })
}

/// Whether any window this frame draws shows the background blur.
fn frame_needs_blur(state: &State) -> bool {
    let translucent = |w: &smithay::desktop::Window| {
        w.toplevel().is_some_and(|t| !surface_opaque(t.wl_surface(), w.geometry()))
    };
    let leaving = state.wm.transition.as_ref().and_then(|t| state.wm.workspaces.get(&t.from)).map(|ws| ws.shown());
    state.space.elements().any(translucent)
        || leaving.is_some_and(|ws| ws.iter().any(translucent))
        || state.closing.iter().any(|c| c.pieces.iter().any(|p| !p.opaque))
}

#[cfg(test)]
mod tests {
    /// The shaders compile only at runtime: make every pipeline here instead
    /// of finding out on screen.
    #[test]
    fn shaders_compile_and_pipelines_build() {
        use objc2_metal::MTLTexture;
        let renderer = super::Renderer::new();
        assert_eq!(renderer.blur_pipelines.len(), 4);
        let targets = renderer.blur_targets((300, 200)).expect("blur textures");
        assert_eq!(targets.levels[1].width(), 150);
        assert_eq!(targets.levels[8].height(), 1);
    }
}
