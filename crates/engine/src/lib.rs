//! The renderer. It owns every wgpu object and builds for wasm32 and native.
//!
//! - `scene`   the engine's mirror of one page, refreshed from `Dirty`.
//! - `camera`  orbit camera, rays, frusta, and the off-axis projection.
//! - `pick`    exact ray picking and work-plane snapping.
//! - `encode`  cull and encode one frame on the CPU.
//! - `render`  the GPU passes. Draws into any texture view.
//! - `demo`    fixture documents, built through a `Replica`.

use std::fmt;

pub mod camera;
pub mod demo;
pub mod encode;
mod mesh;
pub mod pick;
mod render;
mod route;
pub mod scene;

#[cfg(not(target_arch = "wasm32"))]
mod headless;
#[cfg(not(target_arch = "wasm32"))]
pub use headless::{Headless, Offscreen, RenderError, TARGET_FORMAT, render_to_png, write_png};

pub use camera::{Camera, View};
pub use encode::DrawList;
pub use render::{Output, Renderer};
pub use scene::Scene;

/// A configured device and surface, ready to draw a frame.
#[derive(Debug)]
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    list: DrawList,
}

#[derive(Debug)]
pub enum InitError {
    Surface(wgpu::CreateSurfaceError),
    Adapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
    UnsupportedSurface,
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitError::Surface(e) => write!(f, "surface: {e}"),
            InitError::Adapter(e) => write!(f, "adapter: {e}"),
            InitError::Device(e) => write!(f, "device: {e}"),
            InitError::UnsupportedSurface => f.write_str("surface not supported by adapter"),
        }
    }
}

impl std::error::Error for InitError {}

impl Gpu {
    /// Picks WebGPU when the browser exposes it and WebGL2 otherwise.
    pub async fn new(
        target: impl Into<wgpu::SurfaceTarget<'static>>,
        width: u32,
        height: u32,
    ) -> Result<Gpu, InitError> {
        let instance = wgpu::util::new_instance_with_webgpu_detection(
            wgpu::InstanceDescriptor::new_without_display_handle(),
        )
        .await;
        let surface = instance
            .create_surface(target)
            .map_err(InitError::Surface)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                ..Default::default()
            })
            .await
            .map_err(InitError::Adapter)?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("lightdraw"),
                // WebGL2 cannot meet wgpu's default limits.
                required_limits: wgpu::Limits::downlevel_webgl2_defaults()
                    .using_resolution(adapter.limits()),
                ..Default::default()
            })
            .await
            .map_err(InitError::Device)?;
        let config = surface
            .get_default_config(&adapter, width.max(1), height.max(1))
            .ok_or(InitError::UnsupportedSurface)?;
        surface.configure(&device, &config);
        let renderer = Renderer::new(&device, config.format);
        Ok(Gpu {
            surface,
            device,
            queue,
            config,
            renderer,
            list: DrawList::default(),
        })
    }

    #[cfg(target_arch = "wasm32")]
    pub async fn from_canvas(canvas: web_sys::HtmlCanvasElement) -> Result<Gpu, InitError> {
        let (w, h) = (canvas.width(), canvas.height());
        Gpu::new(wgpu::SurfaceTarget::Canvas(canvas), w, h).await
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        self.config.width = width.max(1);
        self.config.height = height.max(1);
        self.surface.configure(&self.device, &self.config);
    }

    /// The next surface texture. None skips the frame: the surface is not
    /// ready, or it was stale and has been reconfigured.
    fn acquire(&self) -> Option<wgpu::SurfaceTexture> {
        match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => Some(t),
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                None
            }
            _ => None,
        }
    }

    /// Fills the surface with one color.
    pub fn clear(&self, color: wgpu::Color) {
        let Some(frame) = self.acquire() else { return };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("clear"),
            });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(color),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }

    /// Draws what `camera` sees of `scene` and presents it.
    pub fn draw(&mut self, scene: &Scene, camera: &Camera) {
        let Some(frame) = self.acquire() else { return };
        let view = frame.texture.create_view(&Default::default());
        let size = [self.config.width, self.config.height];
        self.list.encode(scene, camera, size);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        self.renderer.render(
            &self.device,
            &self.queue,
            &mut encoder,
            Output { view: &view, size },
            camera,
            &self.list,
        );
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
    }
}
