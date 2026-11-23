use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Hologram backdrop: near-black with a cyan cast.
const CLEAR: wgpu::Color = wgpu::Color { r: 0.0, g: 0.04, b: 0.06, a: 1.0 };

#[component]
pub fn App() -> impl IntoView {
    let canvas = NodeRef::<html::Canvas>::new();
    let status = RwSignal::new("starting GPU".to_string());

    Effect::new(move |_| {
        let Some(el) = canvas.get() else { return };
        el.set_width(el.client_width().max(1) as u32);
        el.set_height(el.client_height().max(1) as u32);
        spawn_local(async move {
            match engine::Gpu::from_canvas(el).await {
                Ok(gpu) => {
                    gpu.clear(CLEAR);
                    status.set("GPU ready".into());
                }
                Err(e) => status.set(format!("GPU unavailable: {e}")),
            }
        });
    });

    view! {
        <main class="app">
            <canvas node_ref=canvas class="stage"></canvas>
            <p class="status" data-testid="gpu-status">{move || status.get()}</p>
        </main>
    }
}
