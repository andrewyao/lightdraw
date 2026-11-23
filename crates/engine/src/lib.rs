//! The renderer. It owns every wgpu object and builds for wasm32 and native.

use std::fmt;

pub mod camera;
pub mod demo;
mod route;
pub mod scene;

#[cfg(not(target_arch = "wasm32"))]
mod headless;
#[cfg(not(target_arch = "wasm32"))]
pub use headless::{Headless, TARGET_FORMAT, write_png};

/// A configured device and surface, ready to draw a frame.
#[derive(Debug)]
pub struct Gpu {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
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
        Ok(Gpu {
            surface,
            device,
            queue,
            config,
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

    /// Fills the surface with one color. Skips the frame when the surface is
    /// not ready, and reconfigures when it reports itself stale.
    pub fn clear(&self, color: wgpu::Color) {
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(t)
            | wgpu::CurrentSurfaceTexture::Suboptimal(t) => t,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return;
            }
            _ => return,
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
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
}
