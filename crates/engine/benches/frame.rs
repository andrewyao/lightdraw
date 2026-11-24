//! The 100k-node gate. `cargo bench -p engine`
//!
//! (a) scene build, (b) dirty refresh after moving one frame of 1k children,
//! (c) cull + encode for an orbiting camera, (d) GPU frame time offscreen at
//! 1920x1080, waited to completion. The gate is (c) + (d) under 16.7 ms.
//!
//! One shape in a hundred carries a label. From the overview every label is
//! under the size threshold, so (e) and (f) repeat (c) and (d) from close
//! enough that a few frames' labels draw.

use std::hint::black_box;
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use doc::{Axis, Face, NodeId, Profile};
use engine::demo::Builder;
use engine::{Camera, DrawList, HoloParams, Offscreen, Scene};
use glam::DVec3;

const FRAMES: i32 = 10; // per side, so 100 frames
const CHILDREN: i32 = 999; // per frame, so 100 frames + 99,900 shapes
const PITCH: i32 = 40; // cells between frame origins
const SIZE: [u32; 2] = [1920, 1080];
const LABEL_EVERY: i32 = 100;

struct World {
    b: Builder,
    page: NodeId,
    frames: Vec<NodeId>,
}

fn world() -> World {
    let started = Instant::now();
    let mut b = Builder::new();
    let page = b.page();
    let mut frames = Vec::new();
    let profiles = [
        Profile::Rect,
        Profile::Ellipse,
        Profile::Diamond,
        Profile::Hexagon,
    ];
    for fx in 0..FRAMES {
        for fy in 0..FRAMES {
            let f = b.frame(page, [fx * PITCH, fy * PITCH, 0], [32, 32, 4]);
            for i in 0..CHILDREN {
                let (x, y) = (i % 32, i / 32);
                let h = (i % 3 + 1) as u32;
                let color = [40 + (i % 200) as u8, 180, 255, 255];
                let id = b.shape(
                    f,
                    profiles[i as usize % 4],
                    Axis::Z,
                    [x, y, 0],
                    [1, 1, h],
                    color,
                );
                if i % LABEL_EVERY == 0 {
                    b.label(id, &format!("n{i}"), Face::PosZ);
                }
            }
            frames.push(f);
        }
    }
    eprintln!(
        "built {} nodes through a Replica in {:.1?}",
        frames.len() * (CHILDREN as usize + 1),
        started.elapsed()
    );
    World { b, page, frames }
}

/// Close over the middle frames, near enough that their labels draw.
fn closeup() -> Camera {
    let half = f64::from(FRAMES * PITCH) / 2.0;
    Camera {
        target: DVec3::new(half, half, 0.0),
        yaw: -0.8,
        pitch: 0.9,
        distance: 60.0,
        fov_y: 50f64.to_radians(),
    }
}

/// Sees all 100k nodes: the worst case for cull and encode.
fn overview() -> Camera {
    let half = f64::from(FRAMES * PITCH) / 2.0;
    Camera {
        target: DVec3::new(half, half, 0.0),
        yaw: -0.8,
        pitch: 0.7,
        distance: 700.0,
        fov_y: 50f64.to_radians(),
    }
}

fn bench(c: &mut Criterion) {
    let mut w = world();
    let doc_scene = Scene::build(w.b.doc(), w.page);
    assert_eq!(doc_scene.len(), 100_000);
    let mut probe = DrawList::default();
    probe.encode(&doc_scene, &overview(), SIZE);
    eprintln!(
        "overview camera sees {} instances and {} glyphs",
        probe.len(),
        probe.glyphs.len()
    );
    probe.encode(&doc_scene, &closeup(), SIZE);
    eprintln!(
        "close-up camera sees {} instances and {} glyphs",
        probe.len(),
        probe.glyphs.len()
    );

    let mut g = c.benchmark_group("100k");
    g.sample_size(20).measurement_time(Duration::from_secs(8));

    g.bench_function("a_scene_build", |bench| {
        bench.iter(|| black_box(Scene::build(w.b.doc(), w.page)))
    });

    let mut scene = Scene::build(w.b.doc(), w.page);
    let moved = w.frames[55];
    let mut flip = false;
    g.bench_function("b_refresh_after_moving_a_1k_frame", |bench| {
        bench.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                flip = !flip;
                let z = if flip { 3 } else { 0 };
                let dirty = w.b.move_to(moved, [5 * PITCH, 5 * PITCH, z]);
                let t = Instant::now();
                scene.refresh(w.b.doc(), &dirty);
                total += t.elapsed();
            }
            total
        })
    });

    let mut cam = overview();
    let mut list = DrawList::default();
    g.bench_function("c_cull_encode_orbit", |bench| {
        bench.iter(|| {
            cam.orbit(0.01, 0.0);
            list.encode(&scene, &cam, SIZE);
            black_box(list.len())
        })
    });

    let mut off = Offscreen::new(SIZE).expect("headless GPU");
    let holo = HoloParams::default();
    let mut cam = overview();
    list.encode(&scene, &cam, SIZE);
    off.draw(&list, &cam, &holo);
    let shot = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/snapshots/bench_100k.png");
    engine::write_png(&shot, SIZE[0], SIZE[1], &off.read()).expect("write bench snapshot");
    g.bench_function("d_gpu_frame_1080p", |bench| {
        bench.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                cam.orbit(0.01, 0.0);
                list.encode(&scene, &cam, SIZE);
                let t = Instant::now();
                off.draw(&list, &cam, &holo);
                off.gpu.wait();
                total += t.elapsed();
            }
            total
        })
    });

    let mut cam = closeup();
    g.bench_function("e_cull_encode_closeup_with_labels", |bench| {
        bench.iter(|| {
            cam.orbit(0.01, 0.0);
            list.encode(&scene, &cam, SIZE);
            black_box(list.glyphs.len())
        })
    });

    let mut cam = closeup();
    list.encode(&scene, &cam, SIZE);
    off.draw(&list, &cam, &holo);
    let shot = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/snapshots/bench_closeup.png");
    engine::write_png(&shot, SIZE[0], SIZE[1], &off.read()).expect("write bench snapshot");
    g.bench_function("f_gpu_frame_closeup_with_labels", |bench| {
        bench.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                cam.orbit(0.01, 0.0);
                list.encode(&scene, &cam, SIZE);
                let t = Instant::now();
                off.draw(&list, &cam, &holo);
                off.gpu.wait();
                total += t.elapsed();
            }
            total
        })
    });
    g.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
