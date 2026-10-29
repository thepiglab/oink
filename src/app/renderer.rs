//! GPU trace renderer (wgpu: Metal on macOS, DX12/Vulkan on Windows).
//!
//! All traces are one instanced draw. Each instance is the segment between consecutive points;
//! the vertex shader expands it into a quad with an anti-aliased edge. The point buffer is
//! uploaded only when new trace data arrives. Scrolling, gain and channel spacing are uniforms,
//! so they never touch the points.

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use eframe::egui_wgpu::{self, wgpu};
use oink_eeg::dsp::Point;
use oink_eeg::pipeline::TraceData;

pub const MAX_CHANNELS: usize = 1024;
pub const FLAG_HIDDEN: u32 = 1;
pub const FLAG_HIGHLIGHT: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct Globals {
    /// Plot size in physical pixels.
    pub size: [f32; 2],
    /// Data x (seconds from the data anchor) at the plot's left edge.
    pub x_origin: f32,
    /// Physical pixels per second.
    pub x_scale: f32,
    /// Half line width in physical pixels.
    pub half_width: f32,
    /// 1 if the framebuffer is sRGB (colours must be linearised).
    pub linear: u32,
    pub _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, Pod, Zeroable)]
pub struct ChannelGpu {
    /// Baseline, physical pixels from the plot top.
    pub y0: f32,
    /// Physical pixels per display unit (negative draws positive values upwards).
    pub gain: f32,
    /// RGBA8.
    pub color: u32,
    pub flags: u32,
}

const SHADER: &str = r#"
struct Globals {
    size: vec2<f32>,
    x_origin: f32,
    x_scale: f32,
    half_width: f32,
    linear: u32,
    pad: vec2<f32>,
};
struct Chan {
    y0: f32,
    gain: f32,
    color: u32,
    flags: u32,
};
@group(0) @binding(0) var<uniform> g: Globals;
@group(0) @binding(1) var<uniform> chans: array<Chan, 1024>;

struct VOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) local: vec2<f32>,
    @location(1) @interpolate(flat) len_hw: vec2<f32>,
    @location(2) color: vec4<f32>,
};

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(hi, lo, c <= vec3<f32>(0.04045));
}

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) a: vec2<f32>,
    @location(1) ach: u32,
    @location(2) b: vec2<f32>,
    @location(3) bch: u32,
) -> VOut {
    var out: VOut;
    out.pos = vec4<f32>(-2.0, -2.0, 0.0, 1.0);
    out.local = vec2<f32>(0.0, 0.0);
    out.len_hw = vec2<f32>(0.0, 0.0);
    out.color = vec4<f32>(0.0, 0.0, 0.0, 0.0);
    // Different channels, or a gap marker: no segment.
    if (ach != bch || ach >= 1024u) {
        return out;
    }
    let c = chans[ach];
    if ((c.flags & 1u) != 0u) {
        return out;
    }
    let pa = vec2<f32>((a.x - g.x_origin) * g.x_scale, c.y0 + a.y * c.gain);
    let pb = vec2<f32>((b.x - g.x_origin) * g.x_scale, c.y0 + b.y * c.gain);
    if (max(pa.x, pb.x) < -4.0 || min(pa.x, pb.x) > g.size.x + 4.0) {
        return out;
    }
    var hw = g.half_width;
    if ((c.flags & 2u) != 0u) {
        hw = hw * 1.8;
    }
    let d = pb - pa;
    let len = length(d);
    var dir = vec2<f32>(1.0, 0.0);
    if (len > 0.0001) {
        dir = d / len;
    }
    let n = vec2<f32>(-dir.y, dir.x);
    let w = hw + 1.0;
    var t = -w;
    var s = -w;
    switch vi {
        case 0u: { t = -w; s = -w; }
        case 1u: { t = -w; s = w; }
        case 2u: { t = len + w; s = -w; }
        case 3u: { t = len + w; s = -w; }
        case 4u: { t = -w; s = w; }
        default: { t = len + w; s = w; }
    }
    let p = pa + dir * t + n * s;
    out.pos = vec4<f32>(p.x / g.size.x * 2.0 - 1.0, 1.0 - p.y / g.size.y * 2.0, 0.0, 1.0);
    out.local = vec2<f32>(t, s);
    out.len_hw = vec2<f32>(len, hw);
    var col = unpack4x8unorm(c.color);
    if (g.linear != 0u) {
        col = vec4<f32>(to_linear(col.rgb), col.a);
    }
    out.color = col;
    return out;
}

@fragment
fn fs_main(in: VOut) -> @location(0) vec4<f32> {
    let t = in.local.x;
    let s = in.local.y;
    let len = in.len_hw.x;
    let hw = in.len_hw.y;
    var dist = abs(s);
    if (t < 0.0) {
        dist = length(vec2<f32>(t, s));
    } else if (t > len) {
        dist = length(vec2<f32>(t - len, s));
    }
    let alpha = clamp(hw + 0.5 - dist, 0.0, 1.0);
    if (alpha <= 0.0) {
        discard;
    }
    // Colours are premultiplied.
    return in.color * alpha;
}
"#;

pub struct TraceRenderer {
    pipeline: wgpu::RenderPipeline,
    globals: wgpu::Buffer,
    channels: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    points: wgpu::Buffer,
    capacity: u64,
    count: u32,
    current: Option<Arc<TraceData>>,
    pub linear: bool,
}

impl TraceRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat, samples: u32) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("traces"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let uniform = |binding, visibility| wgpu::BindGroupLayoutEntry {
            binding,
            visibility,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("traces"),
            entries: &[uniform(0, wgpu::ShaderStages::VERTEX_FRAGMENT), uniform(1, wgpu::ShaderStages::VERTEX)],
        });
        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trace globals"),
            size: std::mem::size_of::<Globals>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let channels = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trace channels"),
            size: (MAX_CHANNELS * std::mem::size_of::<ChannelGpu>()) as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("traces"),
            layout: &layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: channels.as_entire_binding() },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("traces"),
            bind_group_layouts: &[&layout],
            push_constant_ranges: &[],
        });
        let attrs_a = wgpu::vertex_attr_array![0 => Float32x2, 1 => Uint32];
        let attrs_b = wgpu::vertex_attr_array![2 => Float32x2, 3 => Uint32];
        let stride = std::mem::size_of::<Point>() as u64;
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("traces"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[
                    wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs_a },
                    wgpu::VertexBufferLayout { array_stride: stride, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs_b },
                ],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleList, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState { count: samples.max(1), ..Default::default() },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });
        let capacity = 64 * 1024;
        let points = Self::point_buffer(device, capacity);
        TraceRenderer {
            pipeline,
            globals,
            channels,
            bind_group,
            points,
            capacity,
            count: 0,
            current: None,
            linear: format.is_srgb(),
        }
    }

    fn point_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("trace points"),
            size,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, cb: &TraceCallback) {
        let mut globals = cb.globals;
        globals.linear = self.linear as u32;
        queue.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        let n = cb.channels.len().min(MAX_CHANNELS);
        if n > 0 {
            queue.write_buffer(&self.channels, 0, bytemuck::cast_slice(&cb.channels[..n]));
        }
        if self.current.as_ref().is_some_and(|c| Arc::ptr_eq(c, &cb.data)) {
            return;
        }
        let bytes: &[u8] = bytemuck::cast_slice(&cb.data.points);
        if bytes.len() as u64 > self.capacity {
            self.capacity = (bytes.len() as u64 * 5 / 4).next_multiple_of(64 * 1024);
            self.points = Self::point_buffer(device, self.capacity);
        }
        if !bytes.is_empty() {
            queue.write_buffer(&self.points, 0, bytes);
        }
        self.count = cb.data.points.len() as u32;
        self.current = Some(cb.data.clone());
    }

    fn paint(&self, pass: &mut wgpu::RenderPass<'static>) {
        if self.count < 2 {
            return;
        }
        let stride = std::mem::size_of::<Point>() as u64;
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(0, self.points.slice(..));
        pass.set_vertex_buffer(1, self.points.slice(stride..));
        pass.draw(0..6, 0..self.count - 1);
    }
}

pub struct TraceCallback {
    pub data: Arc<TraceData>,
    pub globals: Globals,
    pub channels: Vec<ChannelGpu>,
}

impl egui_wgpu::CallbackTrait for TraceCallback {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _screen: &egui_wgpu::ScreenDescriptor,
        _encoder: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        if let Some(r) = resources.get_mut::<TraceRenderer>() {
            r.upload(device, queue, self);
        }
        Vec::new()
    }

    fn paint(
        &self,
        _info: eframe::egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        if let Some(r) = resources.get::<TraceRenderer>() {
            r.paint(pass);
        }
    }
}

/// Packs a premultiplied egui colour for the shader's `unpack4x8unorm`.
pub fn pack(c: eframe::egui::Color32) -> u32 {
    u32::from_le_bytes(c.to_array())
}
