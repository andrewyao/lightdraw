//! Camera control. The engine's camera allows any pitch; the editor keeps the
//! eye above the floor.

use engine::Camera;

use crate::interaction::CamOp;

/// Applies `op`, then keeps the eye at or above the ground: pitch stays in
/// [0, pi/2] and the orbit target never sinks below z = 0.
pub fn apply(cam: &mut Camera, op: CamOp, size: [u32; 2]) {
    match op {
        CamOp::Orbit { yaw, pitch } => cam.orbit(yaw, pitch),
        CamOp::Pan(d) => cam.pan(d, size),
        CamOp::Dolly { factor, px } => cam.dolly_to_cursor(factor, px, size),
        CamOp::Snap(v) => cam.snap(v),
    }
    cam.pitch = cam.pitch.clamp(0.0, std::f64::consts::FRAC_PI_2);
    cam.target.z = cam.target.z.max(0.0);
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
        );
        assert_eq!(cam.pitch, 0.0);
        assert!(cam.eye().z >= 0.0);
    }

    #[test]
    fn panning_down_cannot_sink_the_eye() {
        let mut cam = Camera::default();
        for _ in 0..50 {
            apply(&mut cam, CamOp::Pan([0.0, -400.0]), [800, 600]);
        }
        assert!(cam.eye().z >= 0.0, "eye at {}", cam.eye().z);
    }
}
