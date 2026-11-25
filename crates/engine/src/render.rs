//! GPU side: meshes, pipelines, and the passes of a frame.
//!
//! Scene pass. The floor (lattice and projector disc), the light cone,
//! faces, lines, label glyphs, and remote cursors all add light into an HDR
//! target, so nothing is sorted.
//! Post. A bright pass and downsample chain at half resolution and below,
//! an additive upsample back up the chain, then one composite that adds the
//! bloom, scanlines, flicker, noise, and aberration, and tonemaps to the
//! output view.
//!
//! The flat style keeps the scene pass, minus the projector, and replaces
//! the post chain with a plain encode. See `Style`.
//!
//! Draws into any `TextureView`, so the browser surface and the offscreen
//! path share every line here.

use std::ops::Range;

use doc::Rgba;
use glam::{DVec3, Vec3, Vec4};
use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::encode::{Batch, DrawList, GlyphInstance, Instance};
use crate::holo::HoloParams;
use crate::mesh;
use crate::scene::to_linear;
use crate::text::ATLAS_SIZE;

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.002,
    g: 0.004,
    b: 0.007,
    a: 1.0,
};
const INSTANCE_BYTES: u64 = std::mem::size_of::<Instance>() as u64;
/// Eye-relative position and padding, then linear RGBA.
type MarkerInstance = [f32; 8];
const MARKER_BYTES: u64 = std::mem::size_of::<MarkerInstance>() as u64;
const GLYPH_BYTES: u64 = std::mem::size_of::<GlyphInstance>() as u64;
const HOLO_BYTES: usize = std::mem::size_of::<HoloParams>();
const GLOBALS_BYTES: usize = 16 * 4 + 4 * 4 * 4 + HOLO_BYTES;
/// Matches CONE_SEGMENTS in shader.wgsl, six vertices a segment.
const CONE_VERTICES: u32 = 64 * 6;
/// Half resolution down to 1/64.
const BLOOM_LEVELS: usize = 6;
const COMMON_WGSL: &str = include_str!("common.wgsl");

const ADD: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::One,
        operation: wgpu::BlendOperation::Max,
    },
};

/// Premultiplied alpha over what is already drawn, for the flat style.
const OVER: wgpu::BlendState = wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING;

/// The flat style's pipelines: the same shaders as their hologram twins, but
/// painting over in paint order instead of adding light.
#[derive(Debug)]
struct FlatPipelines {
    face: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    glyph: wgpu::RenderPipeline,
}

#[derive(Debug)]
struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// One rung of the bloom chain.
#[derive(Debug)]
struct Level {
    view: wgpu::TextureView,
    /// Reads the rung above (the HDR target for rung 0) to fill this one.
    down: wgpu::BindGroup,
    /// Reads the rung below to add into this one. None on the last rung.
    up: Option<wgpu::BindGroup>,
}

/// Size-dependent targets, rebuilt on resize.
#[derive(Debug)]
struct Targets {
    size: [u32; 2],
    hdr: wgpu::TextureView,
    depth: wgpu::TextureView,
    bloom: Vec<Level>,
    composite: wgpu::BindGroup,
}

/// The view a frame lands in, and its size in pixels.
#[derive(Copy, Clone, Debug)]
pub struct Output<'a> {
    pub view: &'a wgpu::TextureView,
    pub size: [u32; 2],
}

/// What one frame draws: the encoded scene, the view, and the look.
#[derive(Copy, Clone, Debug)]
pub struct Frame<'a> {
    pub list: &'a DrawList,
    pub camera: &'a Camera,
    pub holo: &'a HoloParams,
}

/// How a frame is shaded.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Style {
    /// Projected light: rim, halos, bloom, scanlines, the projector disc
    /// and cone, all in the look's hue family.
    #[default]
    Hologram,
    /// Plain fills and lines in the documents' own colours on a neutral
    /// grid, with no glow or screen effects. The 2D view.
    Flat,
}

/// A remote participant's 3D cursor, drawn as a small glowing point of
/// fixed screen size.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Marker {
    /// World position in cells.
    pub pos: Vec3,
    pub color: Rgba,
}

#[derive(Debug)]
pub struct Renderer {
    meshes: Vec<Mesh>,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    floor: wgpu::RenderPipeline,
    cone: wgpu::RenderPipeline,
    face: wgpu::RenderPipeline,
    line: wgpu::RenderPipeline,
    marker: wgpu::RenderPipeline,
    glyph: wgpu::RenderPipeline,
    flat: FlatPipelines,
    prefilter: wgpu::RenderPipeline,
    down: wgpu::RenderPipeline,
    up: wgpu::RenderPipeline,
    composite: wgpu::RenderPipeline,
    post_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    instances: wgpu::Buffer,
    staging: Vec<Instance>,
    markers: Vec<Marker>,
    marker_buffer: wgpu::Buffer,
    glyphs: wgpu::Buffer,
    atlas: wgpu::Texture,
    atlas_group: wgpu::BindGroup,
    /// The `Atlas::version` last uploaded.
    atlas_version: u64,
    targets: Option<Targets>,
    style: Style,
}

impl Renderer {
    /// `output` is the format of the views `render` will draw into.
    pub fn new(device: &wgpu::Device, output: wgpu::TextureFormat) -> Renderer {
        let module = |label, body: &str| {
            device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some(label),
                source: wgpu::ShaderSource::Wgsl(format!("{COMMON_WGSL}\n{body}").into()),
            })
        };
        let scene_shader = module("scene", include_str!("shader.wgsl"));
        let post_shader = module("post", include_str!("post.wgsl"));

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: GLOBALS_BYTES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform = wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[uniform],
        });
        let globals_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals.as_entire_binding(),
            }],
        });
        let scene_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene"),
            bind_group_layouts: &[Some(&globals_layout)],
            immediate_size: 0,
        });

        let solid_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 5 => Float32x4];
        let instance_attrs = wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4, 6 => Float32x4];
        let solid_buffers = [
            Some(wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<mesh::Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &solid_attrs,
            }),
            Some(wgpu::VertexBufferLayout {
                array_stride: INSTANCE_BYTES,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &instance_attrs,
            }),
        ];
        // Every scene draw adds light and tests depth; only the floor writes
        // it, so the volume reads through itself but not through the ground.
        let scene_pipeline = |label,
                              vs,
                              fs,
                              buffers: &[Option<wgpu::VertexBufferLayout<'_>>],
                              cull,
                              write_depth,
                              blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&scene_layout),
                vertex: wgpu::VertexState {
                    module: &scene_shader,
                    entry_point: Some(vs),
                    compilation_options: Default::default(),
                    buffers,
                },
                primitive: wgpu::PrimitiveState {
                    cull_mode: cull,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(write_depth),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &scene_shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let floor = scene_pipeline("floor", "vs_floor", "fs_floor", &[], None, true, ADD);
        let cone = scene_pipeline("cone", "vs_cone", "fs_cone", &[], None, false, ADD);
        let face_with = |blend| {
            scene_pipeline(
                "face",
                "vs_solid",
                "fs_face",
                &solid_buffers,
                None,
                false,
                blend,
            )
        };
        let line_with = |blend| {
            scene_pipeline(
                "line",
                "vs_solid",
                "fs_line",
                &solid_buffers,
                Some(wgpu::Face::Back),
                false,
                blend,
            )
        };
        let (face, line) = (face_with(ADD), line_with(ADD));
        let marker_attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4];
        let marker = scene_pipeline(
            "marker",
            "vs_marker",
            "fs_marker",
            &[Some(wgpu::VertexBufferLayout {
                array_stride: MARKER_BYTES,
                step_mode: wgpu::VertexStepMode::Instance,
                attributes: &marker_attrs,
            })],
            None,
            false,
            ADD,
        );

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("linear"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let atlas = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("glyph atlas"),
            size: wgpu::Extent3d {
                width: ATLAS_SIZE,
                height: ATLAS_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("glyph atlas"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let atlas_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("glyph atlas"),
            layout: &atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(
                        &atlas.create_view(&Default::default()),
                    ),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });
        let glyph_attrs = wgpu::vertex_attr_array![
            0 => Float32x4, 1 => Float32x4, 2 => Float32x4, 3 => Float32x4, 4 => Float32x4,
            5 => Float32x4
        ];
        let glyph_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("glyph"),
            bind_group_layouts: &[Some(&globals_layout), Some(&atlas_layout)],
            immediate_size: 0,
        });
        let glyph_with = |blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("glyph"),
                layout: Some(&glyph_layout),
                vertex: wgpu::VertexState {
                    module: &scene_shader,
                    entry_point: Some("vs_glyph"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: GLYPH_BYTES,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &glyph_attrs,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &scene_shader,
                    entry_point: Some("fs_glyph"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: HDR_FORMAT,
                        blend: Some(blend),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let glyph = glyph_with(ADD);
        let flat = FlatPipelines {
            face: face_with(OVER),
            line: line_with(OVER),
            glyph: glyph_with(OVER),
        };

        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[
                uniform,
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture(2),
                texture(3),
            ],
        });
        let post_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("post"),
            bind_group_layouts: &[Some(&post_layout)],
            immediate_size: 0,
        });
        let post_pipeline = |label, fs, format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&post_pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &post_shader,
                    entry_point: Some("vs_post"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &post_shader,
                    entry_point: Some(fs),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        };
        let prefilter = post_pipeline("bloom prefilter", "fs_prefilter", HDR_FORMAT, None);
        let down = post_pipeline("bloom down", "fs_down", HDR_FORMAT, None);
        let up = post_pipeline("bloom up", "fs_up", HDR_FORMAT, Some(ADD));
        // A plain UNORM output, as browsers usually give, needs the encode
        // done in the shader; an sRGB one encodes on store.
        let composite = post_pipeline(
            "composite",
            if output.is_srgb() {
                "fs_composite_linear"
            } else {
                "fs_composite_encode"
            },
            output,
            None,
        );

        let meshes = mesh::PROFILES
            .iter()
            .map(|&p| {
                let data = mesh::build(p);
                Mesh {
                    vertices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mesh vertices"),
                        contents: bytemuck::cast_slice(&data.vertices),
                        usage: wgpu::BufferUsages::VERTEX,
                    }),
                    indices: device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("mesh indices"),
                        contents: bytemuck::cast_slice(&data.indices),
                        usage: wgpu::BufferUsages::INDEX,
                    }),
                    count: data.indices.len() as u32,
                }
            })
            .collect();

        Renderer {
            meshes,
            globals,
            globals_group,
            floor,
            cone,
            face,
            line,
            marker,
            glyph,
            flat,
            prefilter,
            down,
            up,
            composite,
            post_layout,
            sampler,
            instances: instance_buffer(device, 1024),
            staging: Vec::new(),
            markers: Vec::new(),
            marker_buffer: marker_buffer(device, 16),
            glyphs: glyph_buffer(device, 256),
            atlas,
            atlas_group,
            atlas_version: 0,
            targets: None,
            style: Style::default(),
        }
    }

    /// The style every later frame draws in.
    pub fn set_style(&mut self, style: Style) {
        self.style = style;
    }

    /// The cursors every later frame draws, replacing the last set.
    pub fn set_markers(&mut self, markers: &[Marker]) {
        self.markers.clear();
        self.markers.extend_from_slice(markers);
    }

    /// Draws `frame` into `output`. Only records commands; the caller
    /// submits.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: Output<'_>,
        frame: Frame<'_>,
    ) {
        let list = frame.list;
        let size = [output.size[0].max(1), output.size[1].max(1)];
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(self.make_targets(device, size));
        }
        queue.write_buffer(&self.globals, 0, &globals(frame, size, self.style));

        // One upload: each profile's instances, then the lines.
        self.staging.clear();
        let mut ranges = Vec::with_capacity(5);
        for batch in list.solids.iter().chain([&list.lines]) {
            let start = self.staging.len() as u32;
            self.staging.extend_from_slice(batch);
            ranges.push(start..self.staging.len() as u32);
        }
        let needed = self.staging.len() as u64 * INSTANCE_BYTES;
        if needed > self.instances.size() {
            self.instances = instance_buffer(device, needed.next_power_of_two() / INSTANCE_BYTES);
        }
        if !self.staging.is_empty() {
            queue.write_buffer(&self.instances, 0, bytemuck::cast_slice(&self.staging));
        }

        let needed = list.glyphs.len() as u64 * GLYPH_BYTES;
        if needed > self.glyphs.size() {
            self.glyphs = glyph_buffer(device, needed.next_power_of_two() / GLYPH_BYTES);
        }
        if !list.glyphs.is_empty() {
            queue.write_buffer(&self.glyphs, 0, bytemuck::cast_slice(&list.glyphs));
        }
        if list.atlas.version != self.atlas_version && !list.atlas.data.is_empty() {
            queue.write_texture(
                self.atlas.as_image_copy(),
                &list.atlas.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(ATLAS_SIZE),
                    rows_per_image: None,
                },
                self.atlas.size(),
            );
            self.atlas_version = list.atlas.version;
        }

        let eye = frame.camera.eye();
        let markers: Vec<MarkerInstance> = self
            .markers
            .iter()
            .map(|m| {
                let rel = (m.pos.as_dvec3() - eye).as_vec3();
                let [r, g, b, a] = to_linear(m.color);
                [rel.x, rel.y, rel.z, 0.0, r, g, b, a]
            })
            .collect();
        let needed = markers.len() as u64 * MARKER_BYTES;
        if needed > self.marker_buffer.size() {
            self.marker_buffer = marker_buffer(device, needed.next_power_of_two() / MARKER_BYTES);
        }
        if !markers.is_empty() {
            queue.write_buffer(&self.marker_buffer, 0, bytemuck::cast_slice(&markers));
        }

        let t = self.targets.as_ref().expect("made above");
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.hdr,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(BACKGROUND),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &t.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(0.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                ..Default::default()
            });
            pass.set_bind_group(0, &self.globals_group, &[]);
            pass.set_pipeline(&self.floor);
            pass.draw(0..6, 0..1);
            if list.bounds.is_some() && self.style == Style::Hologram {
                pass.set_pipeline(&self.cone);
                pass.draw(0..CONE_VERTICES, 0..1);
            }
            match self.style {
                Style::Hologram => {
                    // Light adds, so batch order is free: each batch whole.
                    let runs = (0..4)
                        .map(Batch::Solid)
                        .chain([Batch::Line])
                        .zip(&ranges)
                        .map(|(batch, r)| (batch, r.clone()))
                        .chain([(Batch::Glyph, 0..list.glyphs.len() as u32)]);
                    for (batch, range) in runs {
                        self.draw_run(&mut pass, batch, range, false);
                    }
                }
                Style::Flat => {
                    for run in &list.runs {
                        let base = match run.batch {
                            Batch::Solid(i) => ranges[i].start,
                            Batch::Line => ranges[4].start,
                            Batch::Glyph => 0,
                        };
                        let range = run.range.start + base..run.range.end + base;
                        self.draw_run(&mut pass, run.batch, range, true);
                    }
                }
            }
            if !markers.is_empty() {
                pass.set_pipeline(&self.marker);
                pass.set_vertex_buffer(0, self.marker_buffer.slice(..));
                pass.draw(0..6, 0..markers.len() as u32);
            }
        }

        let post_pass = |encoder: &mut wgpu::CommandEncoder,
                         label,
                         view: &wgpu::TextureView,
                         load,
                         pipeline: &wgpu::RenderPipeline,
                         group: &wgpu::BindGroup| {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(label),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        };
        let clear = wgpu::LoadOp::Clear(wgpu::Color::BLACK);
        let bloom = if self.style == Style::Hologram {
            &t.bloom[..]
        } else {
            &[]
        };
        for (i, level) in bloom.iter().enumerate() {
            let pipeline = if i == 0 { &self.prefilter } else { &self.down };
            post_pass(
                encoder,
                "bloom down",
                &level.view,
                clear,
                pipeline,
                &level.down,
            );
        }
        for level in bloom.iter().rev() {
            if let Some(up) = &level.up {
                let load = wgpu::LoadOp::Load;
                post_pass(encoder, "bloom up", &level.view, load, &self.up, up);
            }
        }
        post_pass(
            encoder,
            "composite",
            output.view,
            clear,
            &self.composite,
            &t.composite,
        );
    }

    /// One batch's instances in `range`, offset into the shared buffers.
    fn draw_run(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        batch: Batch,
        range: Range<u32>,
        flat: bool,
    ) {
        if range.is_empty() {
            return;
        }
        let (face, line, glyph) = if flat {
            (&self.flat.face, &self.flat.line, &self.flat.glyph)
        } else {
            (&self.face, &self.line, &self.glyph)
        };
        let mesh = match batch {
            Batch::Glyph => {
                pass.set_pipeline(glyph);
                pass.set_bind_group(1, &self.atlas_group, &[]);
                pass.set_vertex_buffer(0, self.glyphs.slice(..));
                pass.draw(0..6, range);
                return;
            }
            Batch::Solid(i) => {
                pass.set_pipeline(face);
                &self.meshes[i]
            }
            // Lines use the rect mesh.
            Batch::Line => {
                pass.set_pipeline(line);
                &self.meshes[0]
            }
        };
        pass.set_vertex_buffer(0, mesh.vertices.slice(..));
        pass.set_vertex_buffer(1, self.instances.slice(..));
        pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint16);
        pass.draw_indexed(0..mesh.count, 0, range);
    }

    fn make_targets(&self, device: &wgpu::Device, size: [u32; 2]) -> Targets {
        let texture = |label, [width, height]: [u32; 2], format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format,
                    usage,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let sampled = wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING;
        let hdr = texture("hdr", size, HDR_FORMAT, sampled);
        let depth = texture(
            "depth",
            size,
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        // One texture per rung, not mips of one: WebGL2 cannot sample one
        // level of a texture while drawing into another.
        let views: Vec<wgpu::TextureView> = (1..=BLOOM_LEVELS)
            .map(|k| texture("bloom", size.map(|v| (v >> k).max(1)), HDR_FORMAT, sampled))
            .collect();
        // The second texture is only read by the composite. A rung being
        // drawn into must not be bound, so the bloom passes bind the HDR
        // target there.
        let group = |src: &wgpu::TextureView, aux: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("post"),
                layout: &self.post_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.globals.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(src),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::TextureView(aux),
                    },
                ],
            })
        };
        let composite = group(&hdr, &views[0]);
        let bloom = (0..views.len())
            .map(|k| Level {
                down: group(if k == 0 { &hdr } else { &views[k - 1] }, &hdr),
                up: views.get(k + 1).map(|below| group(below, &hdr)),
                view: views[k].clone(),
            })
            .collect();
        Targets {
            size,
            hdr,
            depth,
            bloom,
            composite,
        }
    }
}

fn instance_buffer(device: &wgpu::Device, count: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("instances"),
        size: count.max(1) * INSTANCE_BYTES,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn glyph_buffer(device: &wgpu::Device, count: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("glyphs"),
        size: count.max(1) * GLYPH_BYTES,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn marker_buffer(device: &wgpu::Device, count: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("markers"),
        size: count.max(1) * MARKER_BYTES,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

/// `Globals` in common.wgsl.
///
/// The grid is a square centred on the lattice point below the eye, rounded
/// to 10 cells so major lines stay put, and sized to the view distance. The
/// projector sits on the ground under the page's bounds, wide enough to hold
/// them, and its cone rises a little past the tallest thing on it.
fn globals(frame: Frame<'_>, size: [u32; 2], style: Style) -> Vec<u8> {
    let camera = frame.camera;
    let eye = camera.eye();
    let centre = (eye / 10.0).floor() * 10.0 * DVec3::new(1.0, 1.0, 0.0);
    let half = (camera.distance * 6.0).clamp(60.0, 5000.0) as f32;
    let (stage, radius, height) = match frame.list.bounds {
        Some((lo, hi)) => {
            let mid = (lo + hi) / 2.0;
            let radius = ((hi - lo).truncate().length() / 2.0) * 1.05 + 1.0;
            let height = hi.z.max(1.0) * 1.3 + 1.0;
            (DVec3::new(mid.x, mid.y, 0.0), radius, height)
        }
        None => (DVec3::new(camera.target.x, camera.target.y, 0.0), 6.0, 0.0),
    };

    let mut out = Vec::with_capacity(GLOBALS_BYTES);
    let vp = camera.relative_view_projection(size).to_cols_array();
    out.extend_from_slice(bytemuck::cast_slice(&vp));
    let viewport = [eye.z as f32, size[0] as f32, size[1] as f32, 0.0];
    out.extend_from_slice(bytemuck::cast_slice(&viewport));
    let grid = Vec4::from(((centre - eye).as_vec3(), half));
    out.extend_from_slice(bytemuck::cast_slice(&grid.to_array()));
    let stage = Vec4::from(((stage - eye).as_vec3(), radius as f32));
    out.extend_from_slice(bytemuck::cast_slice(&stage.to_array()));
    let flat = f32::from(u8::from(style == Style::Flat));
    out.extend_from_slice(bytemuck::cast_slice(&[height as f32, 0.0, flat, 0.0]));
    out.extend_from_slice(bytemuck::bytes_of(frame.holo));
    debug_assert_eq!(out.len(), GLOBALS_BYTES);
    out
}
