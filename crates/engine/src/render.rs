//! GPU side: meshes, pipelines, and the two passes of a frame.
//!
//! Scene pass. Solids per profile, then lines, then the grid, into an HDR
//! target with reversed depth. Post pass. HDR target to the output view.
//! The hologram look replaces `fs_solid`, the line draw, and `post.wgsl`
//! without touching anything else.
//!
//! Draws into any `TextureView`, so the browser surface and the offscreen
//! path share every line here.

use glam::{DVec3, Vec4};
use wgpu::util::DeviceExt;

use crate::camera::Camera;
use crate::encode::{DrawList, Instance};
use crate::mesh;

pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const BACKGROUND: wgpu::Color = wgpu::Color {
    r: 0.004,
    g: 0.008,
    b: 0.012,
    a: 1.0,
};
const INSTANCE_BYTES: u64 = std::mem::size_of::<Instance>() as u64;
const GLOBALS_BYTES: u64 = 16 * 4 + 4 * 4 * 2;

#[derive(Debug)]
struct Mesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
}

/// Size-dependent targets, rebuilt on resize.
#[derive(Debug)]
struct Targets {
    size: [u32; 2],
    hdr: wgpu::TextureView,
    depth: wgpu::TextureView,
    post: wgpu::BindGroup,
}

/// The view a frame lands in, and its size in pixels.
#[derive(Copy, Clone, Debug)]
pub struct Output<'a> {
    pub view: &'a wgpu::TextureView,
    pub size: [u32; 2],
}

#[derive(Debug)]
pub struct Renderer {
    meshes: Vec<Mesh>,
    globals: wgpu::Buffer,
    globals_group: wgpu::BindGroup,
    solid: wgpu::RenderPipeline,
    grid: wgpu::RenderPipeline,
    post: wgpu::RenderPipeline,
    post_layout: wgpu::BindGroupLayout,
    instances: wgpu::Buffer,
    staging: Vec<Instance>,
    targets: Option<Targets>,
}

impl Renderer {
    /// `output` is the format of the views `render` will draw into.
    pub fn new(device: &wgpu::Device, output: wgpu::TextureFormat) -> Renderer {
        let scene_shader = device.create_shader_module(wgpu::include_wgsl!("shader.wgsl"));
        let post_shader = device.create_shader_module(wgpu::include_wgsl!("post.wgsl"));

        let globals = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: GLOBALS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
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

        let depth = |write: bool| wgpu::DepthStencilState {
            format: DEPTH_FORMAT,
            depth_write_enabled: Some(write),
            depth_compare: Some(wgpu::CompareFunction::GreaterEqual),
            stencil: Default::default(),
            bias: Default::default(),
        };

        let solid_attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
        let instance_attrs =
            wgpu::vertex_attr_array![2 => Float32x4, 3 => Float32x4, 4 => Float32x4];
        let solid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid"),
            layout: Some(&scene_layout),
            vertex: wgpu::VertexState {
                module: &scene_shader,
                entry_point: Some("vs_solid"),
                compilation_options: Default::default(),
                buffers: &[
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
                ],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            depth_stencil: Some(depth(true)),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &scene_shader,
                entry_point: Some("fs_solid"),
                compilation_options: Default::default(),
                targets: &[Some(HDR_FORMAT.into())],
            }),
            multiview_mask: None,
            cache: None,
        });
        let grid = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("grid"),
            layout: Some(&scene_layout),
            vertex: wgpu::VertexState {
                module: &scene_shader,
                entry_point: Some("vs_grid"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: Default::default(),
            depth_stencil: Some(depth(false)),
            multisample: Default::default(),
            fragment: Some(wgpu::FragmentState {
                module: &scene_shader,
                entry_point: Some("fs_grid"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: HDR_FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let post_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("post"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let post = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("post"),
            layout: Some(
                &device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("post"),
                    bind_group_layouts: &[Some(&post_layout)],
                    immediate_size: 0,
                }),
            ),
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
                entry_point: Some(if output.is_srgb() {
                    "fs_post_linear"
                } else {
                    "fs_post_encode"
                }),
                compilation_options: Default::default(),
                targets: &[Some(output.into())],
            }),
            multiview_mask: None,
            cache: None,
        });

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
            solid,
            grid,
            post,
            post_layout,
            instances: instance_buffer(device, 1024),
            staging: Vec::new(),
            targets: None,
        }
    }

    /// Draws `list` as seen by `camera` into `output`. Only records
    /// commands; the caller submits.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: Output<'_>,
        camera: &Camera,
        list: &DrawList,
    ) {
        let size = [output.size[0].max(1), output.size[1].max(1)];
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            self.targets = Some(self.make_targets(device, size));
        }
        queue.write_buffer(&self.globals, 0, &globals(camera, size));

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
            pass.set_pipeline(&self.solid);
            pass.set_vertex_buffer(1, self.instances.slice(..));
            // Lines use the rect mesh.
            let mesh_of = [0, 1, 2, 3, 0];
            for (range, m) in ranges.into_iter().zip(mesh_of) {
                if range.is_empty() {
                    continue;
                }
                let mesh = &self.meshes[m];
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint16);
                pass.draw_indexed(0..mesh.count, 0, range);
            }
            pass.set_pipeline(&self.grid);
            pass.draw(0..6, 0..1);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("post"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.post);
            pass.set_bind_group(0, &t.post, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    fn make_targets(&self, device: &wgpu::Device, size: [u32; 2]) -> Targets {
        let texture = |label, format, usage| {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some(label),
                    size: wgpu::Extent3d {
                        width: size[0],
                        height: size[1],
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
        let hdr = texture(
            "hdr",
            HDR_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        );
        let depth = texture(
            "depth",
            DEPTH_FORMAT,
            wgpu::TextureUsages::RENDER_ATTACHMENT,
        );
        let post = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("post"),
            layout: &self.post_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&hdr),
            }],
        });
        Targets {
            size,
            hdr,
            depth,
            post,
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

/// The grid is a square centred on the lattice point below the eye, rounded
/// to 10 cells so major lines stay put, and sized to the view distance.
fn globals(camera: &Camera, size: [u32; 2]) -> Vec<u8> {
    let eye = camera.eye();
    let centre = (eye / 10.0).floor() * 10.0 * DVec3::new(1.0, 1.0, 0.0);
    let half = (camera.distance * 6.0).clamp(60.0, 5000.0) as f32;
    let light = DVec3::new(0.4, -0.6, 0.7).normalize().as_vec3();
    let mut out = Vec::with_capacity(GLOBALS_BYTES as usize);
    let vp = camera.relative_view_projection(size).to_cols_array();
    out.extend_from_slice(bytemuck::cast_slice(&vp));
    out.extend_from_slice(bytemuck::cast_slice(&light.extend(0.35).to_array()));
    let grid = Vec4::from(((centre - eye).as_vec3(), half));
    out.extend_from_slice(bytemuck::cast_slice(&grid.to_array()));
    out
}
