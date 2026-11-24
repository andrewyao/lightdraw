//! The look lab's model: one slider per `HoloParams` field, and the RON
//! text the panel copies and persists.

use engine::HoloParams;

#[derive(Copy, Clone, Debug)]
pub struct Field {
    pub name: &'static str,
    pub min: f32,
    pub max: f32,
    pub get: fn(&HoloParams) -> f32,
    pub set: fn(&mut HoloParams, f32),
}

macro_rules! fields {
    ($($name:ident: $min:expr, $max:expr;)*) => {
        /// Every tunable field. `time` is the clock, not a tunable.
        pub const FIELDS: &[Field] = &[$(Field {
            name: stringify!($name),
            min: $min,
            max: $max,
            get: |p| p.$name,
            set: |p, v| p.$name = v,
        }),*];
    };
}

fields! {
    hue: 0.0, 1.0;
    saturation: 0.0, 1.0;
    face_alpha: 0.0, 1.0;
    rim_power: 0.5, 8.0;
    rim_intensity: 0.0, 4.0;
    edge_width: 0.0, 4.0;
    edge_intensity: 0.0, 6.0;
    bloom_threshold: 0.0, 2.0;
    bloom_intensity: 0.0, 3.0;
    bloom_radius: 0.0, 1.0;
    scanline_density: 0.0, 600.0;
    scanline_speed: 0.0, 100.0;
    scanline_strength: 0.0, 1.0;
    flicker: 0.0, 0.5;
    chroma_aberration: 0.0, 12.0;
    grid_intensity: 0.0, 2.0;
    disc_intensity: 0.0, 3.0;
    cone_intensity: 0.0, 2.0;
    noise: 0.0, 0.3;
}

/// RON of the tunables, with the clock zeroed so the text is stable.
pub fn to_ron(p: &HoloParams) -> String {
    let p = HoloParams { time: 0.0, ..*p };
    ron::ser::to_string_pretty(&p, ron::ser::PrettyConfig::default())
        .expect("in-memory encode cannot fail")
}

/// Missing fields take their defaults, so older saved text still loads.
pub fn from_ron(text: &str) -> Option<HoloParams> {
    ron::from_str(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_slider_drives_its_own_field_and_survives_ron() {
        for f in FIELDS {
            let mut p = HoloParams::default();
            let v = (f.min + f.max) * 0.37 + 0.011;
            (f.set)(&mut p, v);
            assert_eq!((f.get)(&p), v, "{}", f.name);
            let text = to_ron(&p);
            assert!(
                text.contains(&format!("{}: {v}", f.name)),
                "{}: {text}",
                f.name
            );
            assert_eq!(from_ron(&text), Some(p), "{}", f.name);
        }
    }

    #[test]
    fn every_field_but_the_clock_has_a_slider() {
        let text = to_ron(&HoloParams::default());
        let named = text.lines().filter(|l| l.contains(':')).count();
        assert_eq!(FIELDS.len(), named - 1, "{text}");
    }
}
