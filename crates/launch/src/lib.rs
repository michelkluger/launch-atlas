//! Auto-discovery of candidate launch sites from a DEM.
//!
//! A cell qualifies as a candidate launch when:
//!   * its slope is in `slope_deg_range` (typical paragliding takeoff: 25–35°),
//!   * its surface roughness is below `max_roughness_m` (smooth enough to
//!     inflate a wing on),
//!   * its aspect (downhill direction) is within the user's allowed arc
//!     (e.g. SE through W for thermal sites in the northern Alps).
//!
//! After per-cell filtering we apply spatial deduplication: candidates within
//! `dedupe_radius_m` of a higher-scoring sibling are dropped. This prevents
//! 47 "launches" on one meadow.

use hikefly_core::{CellIdx, Dem, Launch, LaunchKind};
use hikefly_terrain::{SlopeAspect, roughness, slope_aspect};

#[derive(Debug, Clone)]
pub struct DiscoveryParams {
    /// Inclusive slope range in degrees.
    pub slope_deg_range: (f32, f32),
    /// Max RMS roughness (meters) to consider a cell launchable.
    pub max_roughness_m: f32,
    /// Allowed aspect arc (compass degrees). Wraparound supported.
    pub aspect_arc: (f32, f32),
    /// Half-width of the per-launch usable arc, in degrees.
    pub launch_arc_half_width_deg: f32,
    /// Drop a candidate if a higher-scoring one exists within this radius.
    pub dedupe_radius_m: f32,
    /// Maximum number of launches to return after dedup.
    pub max_launches: usize,
}

impl Default for DiscoveryParams {
    fn default() -> Self {
        Self {
            slope_deg_range: (22.0, 38.0),
            max_roughness_m: 1.5,
            aspect_arc: (90.0, 270.0), // E through S to W
            launch_arc_half_width_deg: 45.0,
            dedupe_radius_m: 100.0,
            max_launches: 200,
        }
    }
}

#[derive(Debug, Clone)]
struct Candidate {
    cell: CellIdx,
    altitude_m: f32,
    slope_deg: f32,
    aspect_deg: f32,
    score: f32,
}

pub fn discover(dem: &Dem, params: &DiscoveryParams) -> Vec<Launch> {
    let SlopeAspect {
        slope_deg,
        aspect_deg,
    } = slope_aspect(dem);
    let rough = roughness(dem);

    let mut cands: Vec<Candidate> = Vec::new();
    for r in 0..dem.rows {
        for c in 0..dem.cols {
            let i = dem.idx(r, c);
            let s = slope_deg[i];
            let a = aspect_deg[i];
            let rg = rough[i];
            if !s.is_finite() || !a.is_finite() || !rg.is_finite() {
                continue;
            }
            if s < params.slope_deg_range.0 || s > params.slope_deg_range.1 {
                continue;
            }
            if rg > params.max_roughness_m {
                continue;
            }
            if !aspect_in_arc(a, params.aspect_arc) {
                continue;
            }
            let alt = match dem.get(r, c) {
                Some(z) => z,
                None => continue,
            };
            // Score: prefer mid-range slope (~30°) with low roughness and high altitude.
            // Higher altitude == more glide potential, all else equal.
            let slope_target = 30.0;
            let slope_penalty = ((s - slope_target) / 8.0).powi(2);
            let rough_penalty = (rg / params.max_roughness_m).powi(2);
            let score = alt - 50.0 * slope_penalty - 50.0 * rough_penalty;
            cands.push(Candidate {
                cell: CellIdx::new(r, c),
                altitude_m: alt,
                slope_deg: s,
                aspect_deg: a,
                score,
            });
        }
    }

    cands.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    let mut kept: Vec<Candidate> = Vec::new();
    let r2_dedupe = (params.dedupe_radius_m / dem.cell_size_m).powi(2);
    'outer: for c in cands {
        for k in &kept {
            let dr = c.cell.row as f32 - k.cell.row as f32;
            let dc = c.cell.col as f32 - k.cell.col as f32;
            if dr * dr + dc * dc < r2_dedupe {
                continue 'outer;
            }
        }
        kept.push(c);
        if kept.len() >= params.max_launches {
            break;
        }
    }

    kept.into_iter()
        .enumerate()
        .map(|(i, c)| {
            let half = params.launch_arc_half_width_deg;
            let lo = (c.aspect_deg - half + 360.0) % 360.0;
            let hi = (c.aspect_deg + half) % 360.0;
            Launch {
                id: i as u32,
                cell: c.cell,
                altitude_m: c.altitude_m,
                aspect_deg: c.aspect_deg,
                aspect_window_deg: (lo, hi),
                slope_deg: c.slope_deg,
                kind: LaunchKind::AutoDiscovered,
                warnings: vec![],
            }
        })
        .collect()
}

fn aspect_in_arc(aspect: f32, arc: (f32, f32)) -> bool {
    let (lo, hi) = arc;
    if lo <= hi {
        aspect >= lo && aspect <= hi
    } else {
        // Wraparound, e.g. (315, 45) for N-facing.
        aspect >= lo || aspect <= hi
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hikefly_core::LV95;

    /// Cone with peak at the center, falling off radially.
    fn cone_dem(rows: u32, cols: u32, peak_alt: f32, slope_pct: f32) -> Dem {
        let cr = rows as f32 / 2.0;
        let cc = cols as f32 / 2.0;
        Dem::from_fn(rows, cols, 25.0, LV95::new(0.0, 0.0), |r, c| {
            let dr = r as f32 - cr;
            let dc = c as f32 - cc;
            let dist = (dr * dr + dc * dc).sqrt() * 25.0;
            (peak_alt - dist * slope_pct / 100.0).max(0.0)
        })
    }

    #[test]
    fn discovers_launches_on_a_cone() {
        // Peak 1500m, ~58% slope (~30°). Cone falling off in all directions
        // means launches in every aspect — but our default arc filters to E-W
        // through S, so ~half of them survive.
        let dem = cone_dem(81, 81, 1500.0, 58.0);
        let params = DiscoveryParams::default();
        let launches = discover(&dem, &params);
        assert!(launches.len() > 5, "got {}", launches.len());

        // All discovered should face into the southern half-plane.
        for l in &launches {
            assert!(
                l.aspect_deg >= 90.0 - 5.0 && l.aspect_deg <= 270.0 + 5.0,
                "bad aspect: {}",
                l.aspect_deg
            );
            assert!(l.slope_deg >= 22.0 && l.slope_deg <= 38.0);
        }
    }

    #[test]
    fn dedup_collapses_neighbors() {
        let dem = cone_dem(81, 81, 1500.0, 58.0);
        let mut params = DiscoveryParams::default();
        params.dedupe_radius_m = 500.0;
        let launches = discover(&dem, &params);
        // With 500m dedupe radius the southern half of the cone (~750m
        // arc-distance from peak) shouldn't admit dozens of launches.
        assert!(launches.len() < 20, "expected dedup; got {}", launches.len());
    }

    #[test]
    fn flat_terrain_yields_nothing() {
        let dem = Dem::from_fn(50, 50, 25.0, LV95::new(0.0, 0.0), |_, _| 1000.0);
        let launches = discover(&dem, &DiscoveryParams::default());
        assert!(launches.is_empty());
    }
}
