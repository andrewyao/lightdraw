//! Native offscreen rendering: a device with no surface, texture readback,
//! and PNG output. Tests, examples, and the bench draw through this.

use std::fmt;
use std::path::Path;

use doc::{Doc, Kind, NodeId};

use crate::InitError;
use crate::camera::Camera;
use crate::encode::DrawList;
use crate::holo::HoloParams;
use crate::render::{Frame, Marker, Output, Renderer, Style};
use crate::scene::Scene;

/// A renderer drawing into a texture it can read back.
#[derive(Debug)]
pub struct Offscreen {
    pub gpu: Headless,
    renderer: Renderer,
    target: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Offscreen {
    pub fn new(size: [u32; 2]) -> Result<Offscreen, InitError> {
        let gpu = Headless::new()?;
        let renderer = Renderer::new(&gpu.device, TARGET_FORMAT);
        let target = gpu.target(size[0].max(1), size[1].max(1));
        let view = target.create_view(&Default::default());
        Ok(Offscreen {
            gpu,
            renderer,
            target,
            view,
        })
    }

    pub fn size(&self) -> [u32; 2] {
        [self.target.width(), self.target.height()]
    }

    /// The remote cursors every later `draw` shows, replacing the last set.
    pub fn set_markers(&mut self, markers: &[Marker]) {
        self.renderer.set_markers(markers);
    }

    /// The style every later `draw` uses.
    pub fn set_style(&mut self, style: Style) {
        self.renderer.set_style(style);
    }

    /// Records and submits one frame. Returns without waiting for the GPU.
    pub fn draw(&mut self, list: &DrawList, camera: &Camera, holo: &HoloParams) {
        let mut encoder = self
            .gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        let size = self.size();
        self.renderer.render(
            &self.gpu.device,
            &self.gpu.queue,
            &mut encoder,
            Output {
                view: &self.view,
                size,
            },
            Frame { list, camera, holo },
        );
        self.gpu.queue.submit([encoder.finish()]);
    }

    /// Tightly packed RGBA8, top row first. Waits for every submitted frame.
    pub fn read(&self) -> Vec<u8> {
        self.gpu.read_rgba(&self.target)
    }
}

#[derive(Debug)]
pub enum RenderError {
    Init(InitError),
    /// The document has no page to show.
    NoPage,
    Io(std::io::Error),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RenderError::Init(e) => write!(f, "gpu: {e}"),
            RenderError::NoPage => f.write_str("document has no page"),
            RenderError::Io(e) => write!(f, "write: {e}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// Renders the document's first page as `camera` sees it. Tightly packed
/// RGBA8, top row first.
pub fn render_rgba(
    doc: &Doc,
    camera: &Camera,
    size: [u32; 2],
    holo: &HoloParams,
) -> Result<Vec<u8>, RenderError> {
    let page = doc
        .children(NodeId::ROOT)
        .find(|&p| doc.kind(p) == Some(Kind::Page))
        .ok_or(RenderError::NoPage)?;
    let scene = Scene::build(doc, page);
    let mut off = Offscreen::new(size).map_err(RenderError::Init)?;
    let mut list = DrawList::default();
    list.encode(&scene, camera, off.size());
    off.draw(&list, camera, holo);
    Ok(off.read())
}

/// `render_rgba`, written to a PNG.
pub fn render_to_png(
    doc: &Doc,
    camera: &Camera,
    size: [u32; 2],
    holo: &HoloParams,
    path: &Path,
) -> Result<(), RenderError> {
    let rgba = render_rgba(doc, camera, size, holo)?;
    let [w, h] = size.map(|v| v.max(1));
    write_png(path, w, h, &rgba).map_err(RenderError::Io)
}

/// A device and queue with no window.
#[derive(Debug)]
pub struct Headless {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Headless {
    pub fn new() -> Result<Headless, InitError> {
        pollster::block_on(async {
            let instance =
                wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    ..Default::default()
                })
                .await
                .map_err(InitError::Adapter)?;
            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("lightdraw headless"),
                    required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                        .using_resolution(adapter.limits()),
                    ..Default::default()
                })
                .await
                .map_err(InitError::Device)?;
            Ok(Headless { device, queue })
        })
    }

    /// A texture the renderer can draw into and this can read back.
    pub fn target(&self, width: u32, height: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    /// Blocks until the GPU has finished every submitted command.
    pub fn wait(&self) {
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("device lost while waiting");
    }

    /// Tightly packed RGBA8 rows, top row first.
    pub fn read_rgba(&self, texture: &wgpu::Texture) -> Vec<u8> {
        let (w, h) = (texture.width(), texture.height());
        let row = w * 4;
        let padded =
            row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: u64::from(padded) * u64::from(h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(h),
                },
            },
            texture.size(),
        );
        self.queue.submit([encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |r| {
            r.expect("readback map failed");
        });
        self.wait();
        let mapped = buffer.get_mapped_range(..).expect("mapped above");
        let mut out = Vec::with_capacity((row * h) as usize);
        for y in 0..h as usize {
            let start = y * padded as usize;
            out.extend_from_slice(&mapped[start..start + row as usize]);
        }
        out
    }
}

/// sRGB, so the PNG bytes are what a browser canvas would show.
pub const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

pub fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = png::Encoder::new(file, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .and_then(|mut w| w.write_image_data(rgba))
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cleared_odd_sized_target_reads_back_its_color() {
        let gpu = Headless::new().unwrap();
        let tex = gpu.target(67, 5);
        let view = tex.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::RED),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        gpu.queue.submit([enc.finish()]);
        let px = gpu.read_rgba(&tex);
        assert_eq!(px.len(), 67 * 5 * 4);
        assert!(px.chunks(4).all(|p| p == [255, 0, 0, 255]));
    }
}
