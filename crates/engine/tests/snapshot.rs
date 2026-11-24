//! The offscreen path draws the hologram look: one hue family, light that
//! adds where faces overlap, a rim, bloom, and motion only through `time`.

use doc::library::ComponentDef;
use doc::{Axis, Profile};
use engine::demo::{self, Builder};
use engine::{Camera, HoloParams, render_rgba};
use glam::DVec3;

const SIZE: [u32; 2] = [640, 400];

/// The default look with everything that varies by row or frame turned
/// off, so pixels can be compared across positions and renders.
fn still() -> HoloParams {
    HoloParams {
        scanline_strength: 0.0,
        flicker: 0.0,
        noise: 0.0,
        chroma_aberration: 0.0,
        ..HoloParams::default()
    }
}

struct Shot {
    px: Vec<u8>,
    camera: Camera,
}

impl Shot {
    fn take(b: &Builder, camera: Camera, holo: &HoloParams) -> Shot {
        Shot {
            px: render_rgba(b.doc(), &camera, SIZE, holo).unwrap(),
            camera,
        }
    }

    fn rgb(&self, p: DVec3) -> [u8; 3] {
        let [x, y] = self
            .camera
            .project(p, SIZE)
            .expect("in front of the camera");
        let i = (y as usize * SIZE[0] as usize + x as usize) * 4;
        [self.px[i], self.px[i + 1], self.px[i + 2]]
    }

    /// Mean of the 3x3 pixels around where `p` lands.
    fn luma(&self, p: DVec3) -> f64 {
        let [x, y] = self
            .camera
            .project(p, SIZE)
            .expect("in front of the camera");
        let mut sum = 0.0;
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (x, y) = ((x as i64 + dx) as usize, (y as i64 + dy) as usize);
                let i = (y * SIZE[0] as usize + x) * 4;
                sum += self.px[i..i + 3].iter().map(|&c| f64::from(c)).sum::<f64>() / 3.0;
            }
        }
        sum / 9.0
    }
}

/// Looking along +X, a little from above, at `target`. +Y is screen right.
fn side_camera(target: DVec3) -> Camera {
    Camera {
        target,
        yaw: 0.0,
        pitch: 0.15,
        distance: 24.0,
        fov_y: 50f64.to_radians(),
    }
}

fn one_shape(profile: Profile, at: [i32; 3], size: [u32; 3]) -> Builder {
    let mut b = Builder::new();
    let page = b.page();
    b.shape(page, profile, Axis::Z, at, size, [40, 200, 255, 255]);
    b
}

#[test]
fn every_demo_shape_draws_in_the_cyan_family() {
    let rack = ComponentDef::from_ron(include_str!("../../../library/server_rack.ron")).unwrap();
    let (b, ids) = demo::showcase(&rack);
    let shot = Shot::take(&b, demo::showcase_camera(), &still());

    let luma: Vec<f64> = shot
        .px
        .chunks(4)
        .map(|p| (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0)
        .collect();
    let mean = luma.iter().sum::<f64>() / luma.len() as f64;
    let var = luma.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / luma.len() as f64;
    assert!(var > 100.0, "flat image: variance {var}");

    // Fills of every hue, red and amber included, stay cyan to green-blue.
    let scene = engine::Scene::build(b.doc(), ids.canvas);
    for (name, node) in [
        ("crate", ids.crate_box),
        ("cylinder", ids.cylinder),
        ("hexagon", ids.hex),
        ("pipe", ids.pipe),
        ("tower", ids.tower),
    ] {
        let e = scene.get(&doc::OccId { via: vec![], node }).unwrap();
        let top = DVec3::new(
            (e.min.x + e.max.x) / 2.0,
            (e.min.y + e.max.y) / 2.0,
            e.max.z,
        );
        let [r, g, bl] = shot.rgb(top);
        assert!(
            g > r + 30 && bl > r + 30,
            "{name} is in the cyan family: {:?}",
            [r, g, bl]
        );
    }
}

#[test]
fn a_face_seen_through_another_is_brighter() {
    let front = [-1, 0, 2];
    let mut b = one_shape(Profile::Rect, front, [3, 3, 3]);
    let centre = DVec3::new(0.5, 1.5, 3.5);
    let camera = side_camera(centre);
    let alone = Shot::take(&b, camera, &still()).luma(centre);

    // A second box directly behind the first along the view ray.
    let behind = centre + (centre - camera.eye()).normalize() * 9.0;
    let at = (behind - DVec3::splat(1.5)).round().as_ivec3().to_array();
    let page = b.doc().children(doc::NodeId::ROOT).next().unwrap();
    b.shape(
        page,
        Profile::Rect,
        Axis::Z,
        at,
        [3, 3, 3],
        [40, 200, 255, 255],
    );
    let both = Shot::take(&b, camera, &still()).luma(centre);
    assert!(
        both > alone + 8.0,
        "overlap adds light: {alone} then {both}"
    );
}

#[test]
fn a_cylinder_glows_at_its_rim_more_than_at_its_centre() {
    let b = one_shape(Profile::Ellipse, [-3, -3, 0], [6, 6, 6]);
    let mid = DVec3::new(0.0, 0.0, 3.0);
    let shot = Shot::take(
        &b,
        side_camera(mid),
        &HoloParams {
            bloom_intensity: 0.0,
            cone_intensity: 0.0,
            ..still()
        },
    );
    // The camera looks along +X, so the silhouette is at +-Y.
    let centre = shot.luma(mid);
    let rim = shot.luma(mid + DVec3::new(0.0, 2.85, 0.0));
    assert!(rim > centre + 15.0, "rim {rim}, centre {centre}");
}

#[test]
fn bloom_lights_the_dark_beside_a_bright_edge() {
    let b = one_shape(Profile::Rect, [-2, -2, 0], [4, 4, 4]);
    let camera = side_camera(DVec3::new(0.0, 0.0, 2.0));
    // Just outside the box's right side, in empty space.
    let beside = DVec3::new(-2.0, 2.6, 2.0);
    let off = Shot::take(
        &b,
        camera,
        &HoloParams {
            bloom_intensity: 0.0,
            ..still()
        },
    );
    let on = Shot::take(&b, camera, &still());
    let (off, on) = (off.luma(beside), on.luma(beside));
    assert!(
        on > off + 6.0,
        "bloom spills past the edge: {off} then {on}"
    );
}

#[test]
fn time_alone_animates_the_frame() {
    let b = one_shape(Profile::Hexagon, [-2, -2, 0], [4, 4, 3]);
    let camera = side_camera(DVec3::new(0.0, 0.0, 1.5));
    let at = |time| {
        render_rgba(
            b.doc(),
            &camera,
            SIZE,
            &HoloParams {
                time,
                ..HoloParams::default()
            },
        )
        .unwrap()
    };
    let first = at(1.25);
    assert!(first == at(1.25), "a fixed time renders the same frame");
    assert!(first != at(1.5), "scanlines and flicker move with time");
}
