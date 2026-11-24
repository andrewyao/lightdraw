//! The offscreen path draws the demo document, not just a cleared target.

use std::path::PathBuf;

use doc::library::ComponentDef;
use engine::demo;
use glam::DVec3;

const SIZE: [u32; 2] = [640, 400];

fn decode(path: &PathBuf) -> Vec<u8> {
    let dec = png::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).unwrap()));
    let mut reader = dec.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!([info.width, info.height], SIZE);
    buf
}

#[test]
fn the_demo_snapshot_shows_its_shapes() {
    let rack = ComponentDef::from_ron(include_str!("../../../library/server_rack.ron")).unwrap();
    let (b, ids) = demo::showcase(&rack);
    let camera = demo::showcase_camera();
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("demo_test.png");
    engine::render_to_png(b.doc(), &camera, SIZE, &path).unwrap();
    let px = decode(&path);

    let luma: Vec<f64> = px
        .chunks(4)
        .map(|p| (f64::from(p[0]) + f64::from(p[1]) + f64::from(p[2])) / 3.0)
        .collect();
    let mean = luma.iter().sum::<f64>() / luma.len() as f64;
    let var = luma.iter().map(|l| (l - mean).powi(2)).sum::<f64>() / luma.len() as f64;
    assert!(var > 100.0, "flat image: variance {var}");

    // The pixel at the centre of a shape's top face has that shape's hue.
    let at = |p: DVec3| {
        let [x, y] = camera.project(p, SIZE).unwrap();
        let i = (y as usize * SIZE[0] as usize + x as usize) * 4;
        [px[i], px[i + 1], px[i + 2]]
    };
    let scene = engine::Scene::build(b.doc(), ids.canvas);
    let top = |n| {
        let e = scene
            .get(&doc::OccId {
                via: vec![],
                node: n,
            })
            .unwrap();
        DVec3::new(
            (e.min.x + e.max.x) / 2.0,
            (e.min.y + e.max.y) / 2.0,
            e.max.z,
        )
    };
    let [r, g, bl] = at(top(ids.cylinder));
    assert!(r > g && r > 120, "cylinder top is pink: {:?}", [r, g, bl]);
    let [r, g, bl] = at(top(ids.hex));
    assert!(
        r > bl + 60 && g > bl,
        "hexagon top is amber: {:?}",
        [r, g, bl]
    );
    let [r, g, bl] = at(top(ids.pipe));
    assert!(
        r > g + 60 && r > bl + 60,
        "Y-axis pipe top is red: {:?}",
        [r, g, bl]
    );
    let [r, g, bl] = at(top(ids.crate_box));
    assert!(bl > r + 60, "crate top is cyan: {:?}", [r, g, bl]);
}
