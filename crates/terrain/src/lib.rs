//! Terrain operations: slope, aspect, roughness from a DEM.
//!
//! All grids returned are row-major, same shape as the input DEM. Edge cells and
//! cells whose 3x3 neighborhood contains nodata are NaN.

use hikefly_core::Dem;

/// Per-cell slope and aspect computed via Horn's 3x3 method.
pub struct SlopeAspect {
    /// Slope angle in degrees [0, 90).
    pub slope_deg: Vec<f32>,
    /// Compass aspect of the downhill direction, degrees from N [0, 360).
    /// NaN where slope is effectively zero (no defined aspect).
    pub aspect_deg: Vec<f32>,
}

/// Compute slope and aspect grids using Horn (1981).
///
/// Aspect convention: 0 = north, 90 = east, 180 = south, 270 = west — the
/// compass direction the slope FACES (i.e., the downhill direction).
pub fn slope_aspect(dem: &Dem) -> SlopeAspect {
    let n = dem.cell_count();
    let mut slope = vec![f32::NAN; n];
    let mut aspect = vec![f32::NAN; n];
    let cs = dem.cell_size_m;
    let denom = 8.0 * cs;

    if dem.rows < 3 || dem.cols < 3 {
        return SlopeAspect {
            slope_deg: slope,
            aspect_deg: aspect,
        };
    }

    for r in 1..dem.rows - 1 {
        for c in 1..dem.cols - 1 {
            // Row 0 is northernmost; row+1 is south of row.
            let a = dem.get(r - 1, c - 1);
            let b = dem.get(r - 1, c);
            let cc = dem.get(r - 1, c + 1);
            let d = dem.get(r, c - 1);
            let f = dem.get(r, c + 1);
            let g = dem.get(r + 1, c - 1);
            let h = dem.get(r + 1, c);
            let i = dem.get(r + 1, c + 1);

            let (a, b, cc, d, f, g, h, i) = match (a, b, cc, d, f, g, h, i) {
                (Some(a), Some(b), Some(cc), Some(d), Some(f), Some(g), Some(h), Some(i)) => {
                    (a, b, cc, d, f, g, h, i)
                }
                _ => continue,
            };

            let dz_dx = ((cc + 2.0 * f + i) - (a + 2.0 * d + g)) / denom;
            // dz_dN: positive when terrain rises northward.
            let dz_dn = ((a + 2.0 * b + cc) - (g + 2.0 * h + i)) / denom;

            let mag = (dz_dx * dz_dx + dz_dn * dz_dn).sqrt();
            let slope_rad = mag.atan();
            let idx = dem.idx(r, c);
            slope[idx] = slope_rad.to_degrees();

            if mag < 1e-6 {
                aspect[idx] = f32::NAN;
            } else {
                // Downhill compass direction: east component = -dz_dx, north = -dz_dn.
                let east = -dz_dx;
                let north = -dz_dn;
                let mut deg = east.atan2(north).to_degrees();
                if deg < 0.0 {
                    deg += 360.0;
                }
                aspect[idx] = deg;
            }
        }
    }

    SlopeAspect {
        slope_deg: slope,
        aspect_deg: aspect,
    }
}

/// Roughness = RMS residual of the 3x3 neighborhood altitudes against the
/// best-fit plane through them. A planar slope yields ~0; bumpy or stepped
/// terrain yields larger values (meters).
///
/// Useful for filtering candidate launch sites: smooth slopes are safer to
/// inflate a wing on than bouldery / stepped ones.
pub fn roughness(dem: &Dem) -> Vec<f32> {
    let n = dem.cell_count();
    let mut out = vec![f32::NAN; n];
    if dem.rows < 3 || dem.cols < 3 {
        return out;
    }

    for r in 1..dem.rows - 1 {
        for c in 1..dem.cols - 1 {
            // Collect 9 cells with their (dx, dy) offsets in cell units.
            let mut samples: [(f32, f32, f32); 9] = [(0.0, 0.0, 0.0); 9];
            let mut ok = true;
            let mut k = 0;
            for dr in -1i32..=1 {
                for dc in -1i32..=1 {
                    let v = dem.get((r as i32 + dr) as u32, (c as i32 + dc) as u32);
                    match v {
                        Some(z) => {
                            samples[k] = (dc as f32, -dr as f32, z); // y = -row (north up)
                            k += 1;
                        }
                        None => {
                            ok = false;
                            break;
                        }
                    }
                }
                if !ok {
                    break;
                }
            }
            if !ok {
                continue;
            }

            // Fit z = a + b*x + c*y by least squares (normal equations on a 3-var system).
            let (mut sx, mut sy, mut sz) = (0.0f64, 0.0f64, 0.0f64);
            let (mut sxx, mut syy, mut sxy) = (0.0f64, 0.0f64, 0.0f64);
            let (mut sxz, mut syz) = (0.0f64, 0.0f64);
            for &(x, y, z) in samples.iter() {
                let (x, y, z) = (x as f64, y as f64, z as f64);
                sx += x;
                sy += y;
                sz += z;
                sxx += x * x;
                syy += y * y;
                sxy += x * y;
                sxz += x * z;
                syz += y * z;
            }
            let nf = 9.0f64;
            // Solve the 3x3 system for [a,b,c]:
            // | n   sx   sy  | |a|   |sz |
            // | sx  sxx  sxy | |b| = |sxz|
            // | sy  sxy  syy | |c|   |syz|
            let det = nf * (sxx * syy - sxy * sxy) - sx * (sx * syy - sxy * sy)
                + sy * (sx * sxy - sxx * sy);
            if det.abs() < 1e-12 {
                continue;
            }
            let pa = (sz * (sxx * syy - sxy * sxy) - sx * (sxz * syy - sxy * syz)
                + sy * (sxz * sxy - sxx * syz))
                / det;
            let pb = (nf * (sxz * syy - sxy * syz) - sz * (sx * syy - sxy * sy)
                + sy * (sx * syz - sxz * sy))
                / det;
            let pc = (nf * (sxx * syz - sxy * sxz) - sx * (sx * syz - sxz * sy)
                + sz * (sx * sxy - sxx * sy))
                / det;

            let mut sse = 0.0f64;
            for &(x, y, z) in samples.iter() {
                let pred = pa + pb * x as f64 + pc * y as f64;
                let resid = z as f64 - pred;
                sse += resid * resid;
            }
            let rms = (sse / nf).sqrt() as f32;
            out[dem.idx(r, c)] = rms;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hikefly_core::LV95;

    fn flat_dem() -> Dem {
        Dem::from_fn(5, 5, 25.0, LV95::new(0.0, 0.0), |_, _| 1000.0)
    }

    fn ramp_south_dem() -> Dem {
        // Terrain rises to the north (low row index = high altitude).
        // So slope faces south (downhill = south).
        Dem::from_fn(5, 5, 25.0, LV95::new(0.0, 0.0), |r, _| {
            1000.0 + (4 - r as i32) as f32 * 25.0
        })
    }

    #[test]
    fn flat_terrain_has_zero_slope() {
        let sa = slope_aspect(&flat_dem());
        let center = sa.slope_deg[2 * 5 + 2];
        assert!(center.abs() < 1e-3, "got {center}");
    }

    #[test]
    fn south_ramp_aspect_is_180() {
        let sa = slope_aspect(&ramp_south_dem());
        let aspect = sa.aspect_deg[2 * 5 + 2];
        let slope = sa.slope_deg[2 * 5 + 2];
        assert!(slope > 30.0 && slope < 60.0, "slope: {slope}");
        assert!((aspect - 180.0).abs() < 1.0, "aspect: {aspect}");
    }

    #[test]
    fn flat_terrain_has_zero_roughness() {
        let r = roughness(&flat_dem());
        let center = r[2 * 5 + 2];
        assert!(center.abs() < 1e-3, "got {center}");
    }

    #[test]
    fn planar_ramp_has_zero_roughness() {
        let r = roughness(&ramp_south_dem());
        let center = r[2 * 5 + 2];
        assert!(center.abs() < 1e-3, "got {center}");
    }
}
