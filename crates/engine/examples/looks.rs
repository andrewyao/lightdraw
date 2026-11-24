//! Renders the demo document in every `HoloParams` preset, a contact sheet,
//! and a short orbit so motion can be judged. Into `target/snapshots/`:
//!
//! - `look_<preset>.png` at 1280x800.
//! - `looks_sheet.png`: the default at full size on the left, the other four
//!   at half size in a 2x2 grid on the right, in preset order.
//! - `orbit/0000.png`.. 24 frames over 2 s, and `orbit.mp4` and `orbit.gif`
//!   when `ffmpeg` is on PATH.
//!
//! `cargo run -p engine --example looks`

use std::path::{Path, PathBuf};
use std::process::Command;

use doc::library::ComponentDef;
use engine::{DrawList, HoloParams, Offscreen, Scene, demo, write_png};

const SIZE: [u32; 2] = [1280, 800];
const FRAMES: u32 = 24;
const SECONDS: f32 = 2.0;
/// The orbit's sweep over the whole sequence, in radians.
const SWEEP: f64 = 0.6;
/// Stills are taken a moment in, so the scanline bar is on screen.
const STILL_TIME: f32 = 0.4;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = root.join("target/snapshots");
    let rack = ComponentDef::from_ron(&std::fs::read_to_string(
        root.join("library/server_rack.ron"),
    )?)?;
    let (b, ids) = demo::showcase(&rack);
    let scene = Scene::build(b.doc(), ids.canvas);
    let mut off = Offscreen::new(SIZE)?;
    let mut list = DrawList::default();
    let mut camera = demo::showcase_camera();

    list.encode(&scene, &camera, SIZE);
    let mut stills = Vec::new();
    for (name, holo) in HoloParams::presets() {
        let holo = HoloParams {
            time: STILL_TIME,
            ..holo
        };
        off.draw(&list, &camera, &holo);
        let rgba = off.read();
        let path = out.join(format!("look_{name}.png"));
        write_png(&path, SIZE[0], SIZE[1], &rgba)?;
        println!("{}", path.display());
        stills.push(rgba);
    }
    let sheet = out.join("looks_sheet.png");
    write_png(&sheet, SIZE[0] * 2, SIZE[1], &contact_sheet(&stills))?;
    println!("{}", sheet.display());

    let frames = out.join("orbit");
    std::fs::create_dir_all(&frames)?;
    for entry in std::fs::read_dir(&frames)? {
        std::fs::remove_file(entry?.path())?;
    }
    let start = camera.yaw;
    for i in 0..FRAMES {
        let t = i as f32 / FRAMES as f32;
        camera.yaw = start + SWEEP * f64::from(t);
        list.encode(&scene, &camera, SIZE);
        let holo = HoloParams {
            time: t * SECONDS,
            ..HoloParams::default()
        };
        off.draw(&list, &camera, &holo);
        write_png(
            &frames.join(format!("{i:04}.png")),
            SIZE[0],
            SIZE[1],
            &off.read(),
        )?;
    }
    println!("{}", frames.display());
    encode_video(&out, &frames);
    Ok(())
}

/// The first still at full size, then the rest halved into a 2x2 grid to its
/// right.
fn contact_sheet(stills: &[Vec<u8>]) -> Vec<u8> {
    let [w, h] = SIZE.map(|v| v as usize);
    let sheet_w = w * 2;
    let mut sheet = vec![0u8; sheet_w * h * 4];
    for y in 0..h {
        let row = &stills[0][y * w * 4..(y + 1) * w * 4];
        sheet[y * sheet_w * 4..y * sheet_w * 4 + w * 4].copy_from_slice(row);
    }
    for (k, still) in stills[1..].iter().enumerate().take(4) {
        let (ox, oy) = (w + (k % 2) * w / 2, (k / 2) * h / 2);
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                for c in 0..4 {
                    let at = |dx: usize, dy: usize| {
                        u32::from(still[((2 * y + dy) * w + 2 * x + dx) * 4 + c])
                    };
                    let mean = (at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1)) / 4;
                    sheet[((oy + y) * sheet_w + ox + x) * 4 + c] = mean as u8;
                }
            }
        }
    }
    sheet
}

fn encode_video(out: &Path, frames: &Path) {
    let fps = (FRAMES as f32 / SECONDS).to_string();
    let input = frames.join("%04d.png");
    let run = |args: &[&str], name: &str| {
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-framerate", &fps, "-i"])
            .arg(&input)
            .args(args)
            .arg(out.join(name))
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("{}", out.join(name).display());
        } else {
            eprintln!("skipped {name}: ffmpeg missing or failed");
        }
    };
    run(&["-pix_fmt", "yuv420p", "-c:v", "libx264"], "orbit.mp4");
    run(
        &[
            "-vf",
            "scale=640:-1:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
        ],
        "orbit.gif",
    );
}
