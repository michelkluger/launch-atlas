/// Swiss LV95 coordinate (CH1903+ / EPSG:2056). Easting/northing in meters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LV95 {
    pub e: f64,
    pub n: f64,
}

impl LV95 {
    pub fn new(e: f64, n: f64) -> Self {
        Self { e, n }
    }
}

/// Row/column index into a raster grid. (0,0) is top-left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CellIdx {
    pub row: u32,
    pub col: u32,
}

impl CellIdx {
    pub fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }
}

/// Normalize an angle in degrees to [0, 360).
pub fn norm_deg(a: f32) -> f32 {
    let m = a.rem_euclid(360.0);
    if m < 0.0 {
        m + 360.0
    } else {
        m
    }
}

/// Smallest signed difference `a - b` in degrees, in (-180, 180].
pub fn angle_diff_deg(a: f32, b: f32) -> f32 {
    let d = norm_deg(a - b);
    if d > 180.0 {
        d - 360.0
    } else {
        d
    }
}

/// Is `angle` inside the arc `[lo, hi]` (allowing wraparound), with `tolerance` degrees of slop?
pub fn angle_in_arc(angle: f32, lo: f32, hi: f32, tolerance: f32) -> bool {
    let center = norm_deg((lo + hi) * 0.5);
    let half_width = ((norm_deg(hi - lo)) * 0.5) + tolerance;
    angle_diff_deg(angle, center).abs() <= half_width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_deg_handles_negative() {
        assert!((norm_deg(-10.0) - 350.0).abs() < 1e-4);
        assert!((norm_deg(370.0) - 10.0).abs() < 1e-4);
    }

    #[test]
    fn angle_diff_is_signed_and_short() {
        assert!((angle_diff_deg(10.0, 350.0) - 20.0).abs() < 1e-4);
        assert!((angle_diff_deg(350.0, 10.0) + 20.0).abs() < 1e-4);
    }

    #[test]
    fn angle_in_arc_basic() {
        assert!(angle_in_arc(180.0, 135.0, 225.0, 0.0));
        assert!(!angle_in_arc(0.0, 135.0, 225.0, 0.0));
        assert!(angle_in_arc(120.0, 135.0, 225.0, 30.0));
    }
}
