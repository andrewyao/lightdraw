//! Prototype B, "stacked floors": the work plane is locked to a horizontal
//! floor at a fixed height, and PageUp/PageDown change floor. Everything B
//! needs lives here, so deleting this module and its call sites removes it.

use doc::{CellVec, Face, WorkPlane};
use glam::DVec3;

/// Cells between floors.
pub const STOREY: i32 = 4;

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Floors {
    pub level: i32,
}

impl Floors {
    pub fn plane(self) -> WorkPlane {
        WorkPlane {
            origin: CellVec::new(0, 0, self.z()),
            normal: Face::PosZ,
        }
    }

    pub fn z(self) -> i32 {
        self.level.saturating_mul(STOREY)
    }

    pub fn up(self) -> Floors {
        Floors {
            level: self.level.saturating_add(1),
        }
    }

    /// Never below the ground floor.
    pub fn down(self) -> Floors {
        Floors {
            level: (self.level - 1).max(0),
        }
    }

    /// The floor's outline square, `half` cells from `center`, for the overlay.
    pub fn outline(self, center: DVec3, half: f64) -> [DVec3; 4] {
        let z = f64::from(self.z());
        let (x, y) = (center.x.round(), center.y.round());
        [
            DVec3::new(x - half, y - half, z),
            DVec3::new(x + half, y - half, z),
            DVec3::new(x + half, y + half, z),
            DVec3::new(x - half, y + half, z),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floors_step_by_one_storey_and_stop_at_the_ground() {
        let f = Floors::default().up().up();
        assert_eq!(f.plane().origin.z.0, 2 * STOREY);
        assert_eq!(f.plane().normal, Face::PosZ);
        assert_eq!(Floors::default().down(), Floors::default());
    }
}
