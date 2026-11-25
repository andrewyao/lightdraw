//! Camera control and the 2D/3D switch. The engine's camera allows any pose;
//! the editor keeps 2D looking straight down and 3D above the floor.

use engine::{Camera, Projection, Style, View};
use std::f64::consts::FRAC_PI_2;

use crate::interaction::CamOp;

/// 2D is a flat drawing view straight down onto the ground. 3D adds orbit,
/// the axis views, push/pull, and the hologram look.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Dimension {
    #[default]
    Two,
    Three,
}

impl Dimension {
    pub fn style(self) -> Style {
        match self {
            Dimension::Two => Style::Flat,
            Dimension::Three => Style::Hologram,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Dimension::Two => "2D",
            Dimension::Three => "3D",
        }
    }

    pub fn other(self) -> Dimension {
        match self {
            Dimension::Two => Dimension::Three,
            Dimension::Three => Dimension::Two,
        }
    }
}

/// The 3D orientation to come back to after a stretch in 2D.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Orbit {
    pub yaw: f64,
    pub pitch: f64,
}

impl Orbit {
    pub fn of(cam: &Camera) -> Orbit {
        Orbit {
            yaw: cam.yaw,
            pitch: cam.pitch,
        }
    }
}

/// Applies `op`, then holds the camera to `dim`'s rules.
pub fn apply(cam: &mut Camera, op: CamOp, size: [u32; 2], dim: Dimension) {
    match op {
        CamOp::Orbit { yaw, pitch } => cam.orbit(yaw, pitch),
        CamOp::Pan(d) => cam.pan(d, size),
        CamOp::Dolly { factor, px } => cam.dolly_to_cursor(factor, px, size),
        CamOp::Snap(v) => cam.snap(v),
    }
    settle(cam, dim);
}

/// 2D: orthographic, straight down, +Y up on screen. 3D: perspective with
/// the eye at or above the ground: pitch in [0, pi/2] and the orbit target
/// never below z = 0.
pub fn settle(cam: &mut Camera, dim: Dimension) {
    match dim {
        Dimension::Two => {
            cam.snap(View::Top);
            cam.projection = Projection::Orthographic;
        }
        Dimension::Three => {
            cam.projection = Projection::Perspective;
            cam.pitch = cam.pitch.clamp(0.0, FRAC_PI_2);
        }
    }
    cam.target.z = cam.target.z.max(0.0);
}

/// Moves `cam` into `to`. Leaving 3D returns the orientation to restore on
/// the way back; entering 3D restores `resume`. Target and zoom carry over.
pub fn switch(cam: &mut Camera, to: Dimension, resume: Orbit) -> Orbit {
    let left = Orbit::of(cam);
    if to == Dimension::Three {
        cam.yaw = resume.yaw;
        cam.pitch = resume.pitch;
    }
    settle(cam, to);
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orbiting_down_stops_at_the_floor() {
        let mut cam = Camera::default();
        apply(
            &mut cam,
            CamOp::Orbit {
                yaw: 0.0,
                pitch: -3.0,
            },
            [800, 600],
            Dimension::Three,
        );
        assert_eq!(cam.pitch, 0.0);
        assert!(cam.eye().z >= 0.0);
    }

    #[test]
    fn panning_down_cannot_sink_the_eye() {
        let mut cam = Camera::default();
        for _ in 0..50 {
            apply(
                &mut cam,
                CamOp::Pan([0.0, -400.0]),
                [800, 600],
                Dimension::Three,
            );
        }
        assert!(cam.eye().z >= 0.0, "eye at {}", cam.eye().z);
    }

    #[test]
    fn no_camera_op_tilts_the_2d_view() {
        let mut cam = Camera::default();
        settle(&mut cam, Dimension::Two);
        let top = (cam.yaw, cam.pitch);
        for op in [
            CamOp::Orbit {
                yaw: 0.4,
                pitch: -0.7,
            },
            CamOp::Snap(View::Front),
            CamOp::Pan([30.0, -20.0]),
            CamOp::Dolly {
                factor: 0.5,
                px: [100.0, 80.0],
            },
        ] {
            apply(&mut cam, op, [800, 600], Dimension::Two);
            assert_eq!((cam.yaw, cam.pitch), top, "after {op:?}");
            assert_eq!(cam.projection, Projection::Orthographic);
        }
    }

    #[test]
    fn a_round_trip_through_2d_restores_the_3d_view() {
        let mut cam = Camera {
            yaw: 0.9,
            pitch: 0.4,
            ..Camera::default()
        };
        let was = Orbit::of(&cam);
        let resume = switch(&mut cam, Dimension::Two, was);
        assert_eq!(cam.projection, Projection::Orthographic);
        apply(
            &mut cam,
            CamOp::Pan([50.0, 0.0]),
            [800, 600],
            Dimension::Two,
        );
        let target = cam.target;
        switch(&mut cam, Dimension::Three, resume);
        assert_eq!((cam.yaw, cam.pitch), (0.9, 0.4));
        assert_eq!(cam.projection, Projection::Perspective);
        assert_eq!(cam.target, target, "the pan carries over");
    }
}
