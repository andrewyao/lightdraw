//! The hologram look's tunables. One value drives every frame, so a look-lab
//! slider can change any of them live without touching the scene.

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};

/// `repr(C)` and all `f32`: the GPU reads it byte for byte as `Holo` in
/// common.wgsl, so the field order there must match.
#[repr(C)]
#[derive(Copy, Clone, Debug, PartialEq, Pod, Zeroable, Serialize, Deserialize)]
#[serde(default)]
pub struct HoloParams {
    /// Base hue of the family, in turns: 0.5 is cyan.
    pub hue: f32,
    pub saturation: f32,
    /// How much light a face adds where it is seen head-on.
    pub face_alpha: f32,
    /// Fresnel exponent. Higher keeps the rim tighter to the silhouette.
    pub rim_power: f32,
    pub rim_intensity: f32,
    /// Edge line half-width, in pixels.
    pub edge_width: f32,
    pub edge_intensity: f32,
    /// HDR luminance where bloom starts.
    pub bloom_threshold: f32,
    pub bloom_intensity: f32,
    /// 0 keeps bloom tight; toward 1 each coarser level adds more spread.
    pub bloom_radius: f32,
    /// Scanlines per screen height.
    pub scanline_density: f32,
    /// Scanlines per second, drifting upward.
    pub scanline_speed: f32,
    pub scanline_strength: f32,
    pub flicker: f32,
    /// Channel offset at the screen corners, in pixels at 1080 lines.
    pub chroma_aberration: f32,
    pub grid_intensity: f32,
    pub disc_intensity: f32,
    pub cone_intensity: f32,
    pub noise: f32,
    /// Seconds. Animates scanlines, flicker, and noise; nothing else.
    pub time: f32,
}

impl Default for HoloParams {
    fn default() -> HoloParams {
        HoloParams {
            hue: 0.52,
            saturation: 0.85,
            face_alpha: 0.32,
            rim_power: 2.5,
            rim_intensity: 1.5,
            edge_width: 1.1,
            edge_intensity: 1.6,
            bloom_threshold: 0.6,
            bloom_intensity: 0.9,
            bloom_radius: 0.65,
            scanline_density: 220.0,
            scanline_speed: 18.0,
            scanline_strength: 0.18,
            flicker: 0.04,
            chroma_aberration: 3.0,
            grid_intensity: 0.35,
            disc_intensity: 1.0,
            cone_intensity: 0.35,
            noise: 0.035,
            time: 0.0,
        }
    }
}

impl HoloParams {
    /// Named starting points for the look lab, the default first.
    pub fn presets() -> [(&'static str, HoloParams); 5] {
        let base = HoloParams::default();
        [
            ("default", base),
            (
                "cyan_classic",
                HoloParams {
                    hue: 0.5,
                    saturation: 0.9,
                    face_alpha: 0.18,
                    rim_intensity: 1.2,
                    scanline_strength: 0.3,
                    ..base
                },
            ),
            (
                "amber_retro",
                HoloParams {
                    hue: 0.09,
                    saturation: 0.9,
                    face_alpha: 0.1,
                    edge_intensity: 1.8,
                    scanline_density: 160.0,
                    scanline_strength: 0.45,
                    flicker: 0.1,
                    chroma_aberration: 5.0,
                    noise: 0.08,
                    ..base
                },
            ),
            (
                "minimal_clean",
                HoloParams {
                    saturation: 0.45,
                    face_alpha: 0.06,
                    rim_intensity: 0.5,
                    edge_width: 0.8,
                    edge_intensity: 1.6,
                    bloom_intensity: 0.35,
                    bloom_radius: 0.5,
                    scanline_strength: 0.0,
                    flicker: 0.0,
                    chroma_aberration: 0.0,
                    grid_intensity: 0.3,
                    cone_intensity: 0.12,
                    noise: 0.0,
                    ..base
                },
            ),
            (
                "intense_neon",
                HoloParams {
                    hue: 0.83,
                    saturation: 1.0,
                    face_alpha: 0.32,
                    rim_power: 1.8,
                    rim_intensity: 1.6,
                    edge_width: 1.4,
                    edge_intensity: 3.5,
                    bloom_threshold: 0.4,
                    bloom_intensity: 1.6,
                    bloom_radius: 0.9,
                    cone_intensity: 0.6,
                    disc_intensity: 1.5,
                    ..base
                },
            ),
        ]
    }
}
