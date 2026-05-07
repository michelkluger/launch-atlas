use crate::geom::CellIdx;

pub type LaunchId = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    /// SHV / ParaglidingEarth listed.
    Official,
    /// Known hike-and-fly spot (community-curated).
    HikeAndFly,
    /// Synthesized from terrain analysis.
    AutoDiscovered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WarningSeverity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone)]
pub struct Warning {
    pub severity: WarningSeverity,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct Launch {
    pub id: LaunchId,
    pub cell: CellIdx,
    pub altitude_m: f32,
    /// Direction the slope faces, degrees from N (0=N, 90=E, 180=S, 270=W).
    pub aspect_deg: f32,
    /// Usable launch arc, half-open. (lo, hi) measured clockwise from N.
    pub aspect_window_deg: (f32, f32),
    pub slope_deg: f32,
    pub kind: LaunchKind,
    pub warnings: Vec<Warning>,
}

impl Launch {
    pub fn aspect_window_half_width(&self) -> f32 {
        let (lo, hi) = self.aspect_window_deg;
        let width = crate::geom::norm_deg(hi - lo);
        width * 0.5
    }
}
