//! Single-source glide-reachability flood.
//!
//! Given a launch (cell, altitude) and a glider model (still-air glide ratio,
//! safety buffer above terrain), compute for every reachable ground cell the
//! maximum altitude MSL we can be at when overflying it. Cells where that
//! altitude would fall below `terrain + safety_buffer_m` are unreachable.
//!
//! Algorithm: Dijkstra on a max-heap. Edges are 8-connected with cost in the
//! "altitude lost" sense — to glide from A to B at horizontal distance `d`
//! costs `d / glide_ratio` meters of altitude. We want each cell's MAXIMUM
//! reachable MSL altitude, equivalent to MIN altitude lost from launch.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use hikefly_core::{CellIdx, Dem, Launch, Wind};

#[derive(Debug, Clone, Copy)]
pub struct GlideParams {
    /// Still-air glide ratio (e.g., 8.0 for an EN-A/B at trim).
    pub glide_ratio: f32,
    /// Minimum AGL clearance to call a cell reachable, ENROUTE (meters).
    pub safety_buffer_m: f32,
    /// AGL we already have once airborne. Steep slopes drop away fast at
    /// takeoff so this is what lets the first dozen meters of glide actually
    /// build clearance before the enroute buffer applies.
    pub takeoff_initial_agl_m: f32,
    /// Best-glide airspeed (m/s). Typical paraglider trim ~10 m/s. Sink rate
    /// is derived as `airspeed / glide_ratio` so that still-air ground glide
    /// ratio equals `glide_ratio`.
    pub airspeed_ms: f32,
    /// Wind affecting the glide path. None = still air.
    pub wind: Option<Wind>,
}

impl Default for GlideParams {
    fn default() -> Self {
        Self {
            glide_ratio: 8.0,
            safety_buffer_m: 15.0,
            takeoff_initial_agl_m: 10.0,
            airspeed_ms: 10.0,
            wind: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FloodResult {
    /// Max MSL altitude reachable per cell (NaN = unreached). Same shape as DEM.
    pub max_alt_msl: Vec<f32>,
    /// Furthest reached cell (great-circle approx; cell-grid Euclidean here).
    pub max_distance_m: f32,
    /// Cell at the maximum-distance position from launch (None if nothing reached).
    pub farthest_cell: Option<CellIdx>,
    /// Number of reached cells (excluding launch).
    pub reachable_cells: u32,
    /// Total reachable area in m^2.
    pub area_m2: f64,
}

/// Lightweight result without the per-cell altitude grid. Use for ranking
/// many launches without paying ~32 MB / launch / glide-ratio in RAM.
#[derive(Debug, Clone)]
pub struct FloodSummary {
    pub max_distance_m: f32,
    pub farthest_cell: Option<CellIdx>,
    pub reachable_cells: u32,
    pub area_m2: f64,
}

/// Run a flood and return only the summary stats; the per-cell grid is
/// dropped before this returns.
pub fn flood_summary(dem: &Dem, launch: &Launch, p: &GlideParams) -> FloodSummary {
    let r = flood(dem, launch, p);
    FloodSummary {
        max_distance_m: r.max_distance_m,
        farthest_cell: r.farthest_cell,
        reachable_cells: r.reachable_cells,
        area_m2: r.area_m2,
    }
}

/// Internal max-heap entry: scaled altitude (mm) + cell.
#[derive(Debug)]
struct Node {
    alt_mm: i32,
    cell: CellIdx,
}

impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.alt_mm == other.alt_mm
    }
}
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Node {
    fn cmp(&self, other: &Self) -> Ordering {
        self.alt_mm.cmp(&other.alt_mm)
    }
}

const NEIGHBORS: [(i32, i32, f32); 8] = [
    (-1, 0, 1.0),
    (1, 0, 1.0),
    (0, -1, 1.0),
    (0, 1, 1.0),
    (-1, -1, std::f32::consts::SQRT_2),
    (-1, 1, std::f32::consts::SQRT_2),
    (1, -1, std::f32::consts::SQRT_2),
    (1, 1, std::f32::consts::SQRT_2),
];

pub fn flood(dem: &Dem, launch: &Launch, p: &GlideParams) -> FloodResult {
    let n = dem.cell_count();
    let mut max_alt = vec![f32::NAN; n];
    let cs = dem.cell_size_m;

    // Sink rate is fixed (airspeed / still-air GR). Tailwind shifts ground
    // speed and therefore ground glide ratio per direction.
    let sink_ms = (p.airspeed_ms / p.glide_ratio.max(0.1)).max(0.01);
    let (wind_to_rad, wind_ms) = match p.wind {
        Some(w) if w.speed_kmh > 0.0 => {
            ((w.from_deg + 180.0).to_radians(), w.speed_kmh / 3.6)
        }
        _ => (0.0, 0.0),
    };

    // Per-neighbor effective ground glide ratio. NaN = headwind kills any
    // forward progress; we won't propagate in that direction.
    let mut effective_gr = [f32::NAN; 8];
    for i in 0..8 {
        let (dr, dc, _) = NEIGHBORS[i];
        let glide_dir_rad = (dc as f32).atan2(-(dr as f32));
        let tailwind_ms = if wind_ms > 0.0 {
            wind_ms * (glide_dir_rad - wind_to_rad).cos()
        } else {
            0.0
        };
        let ground_speed = p.airspeed_ms + tailwind_ms;
        effective_gr[i] = if ground_speed < 0.5 {
            f32::NAN
        } else {
            ground_speed / sink_ms
        };
    }

    let launch_alt = launch.altitude_m + p.takeoff_initial_agl_m;
    let li = dem.idx(launch.cell.row, launch.cell.col);
    max_alt[li] = launch_alt;

    let mut heap = BinaryHeap::new();
    heap.push(Node {
        alt_mm: (launch_alt * 1000.0) as i32,
        cell: launch.cell,
    });

    let mut max_dist = 0.0f32;
    let mut farthest: Option<CellIdx> = None;
    let mut reachable = 0u32;

    while let Some(Node { alt_mm, cell }) = heap.pop() {
        let cur = alt_mm as f32 / 1000.0;
        let ci = dem.idx(cell.row, cell.col);
        if max_alt[ci] > cur + 1e-3 {
            continue;
        }

        for (i, &(dr, dc, dist_factor)) in NEIGHBORS.iter().enumerate() {
            let gr = effective_gr[i];
            if !gr.is_finite() {
                continue;
            }
            let nr = cell.row as i32 + dr;
            let nc = cell.col as i32 + dc;
            if !dem.in_bounds(nr, nc) {
                continue;
            }
            let neighbor = CellIdx::new(nr as u32, nc as u32);
            let ni = dem.idx(neighbor.row, neighbor.col);
            let terrain = dem.data[ni];
            if !terrain.is_finite() {
                continue;
            }
            let alt_lost = (cs * dist_factor) / gr;
            let new_alt = cur - alt_lost;
            if new_alt - terrain < p.safety_buffer_m {
                continue;
            }
            let prev = max_alt[ni];
            let improved = !prev.is_finite() || new_alt > prev + 1e-2;
            if !improved {
                continue;
            }
            if !prev.is_finite() {
                reachable += 1;
                let dr2 = (neighbor.row as i32 - launch.cell.row as i32) as f32;
                let dc2 = (neighbor.col as i32 - launch.cell.col as i32) as f32;
                let d = (dr2 * dr2 + dc2 * dc2).sqrt() * cs;
                if d > max_dist {
                    max_dist = d;
                    farthest = Some(neighbor);
                }
            }
            max_alt[ni] = new_alt;
            heap.push(Node {
                alt_mm: (new_alt * 1000.0) as i32,
                cell: neighbor,
            });
        }
    }

    FloodResult {
        max_alt_msl: max_alt,
        max_distance_m: max_dist,
        farthest_cell: farthest,
        reachable_cells: reachable,
        area_m2: reachable as f64 * dem.area_m2(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hikefly_core::{LV95, LaunchKind, launch::Launch};

    fn flat_low_dem(rows: u32, cols: u32) -> Dem {
        Dem::from_fn(rows, cols, 25.0, LV95::new(0.0, 0.0), |_, _| 0.0)
    }

    fn synthetic_launch(row: u32, col: u32, alt: f32) -> Launch {
        Launch {
            id: 1,
            cell: CellIdx::new(row, col),
            altitude_m: alt,
            aspect_deg: 180.0,
            aspect_window_deg: (135.0, 225.0),
            slope_deg: 30.0,
            kind: LaunchKind::AutoDiscovered,
            warnings: vec![],
        }
    }

    #[test]
    fn flat_terrain_reach_matches_glide_cone() {
        // From altitude H over flat ground at altitude 0 with glide ratio GR,
        // the reachable radius is approximately H * GR (minus the safety buffer).
        let dem = flat_low_dem(81, 81);
        let launch = synthetic_launch(40, 40, 1000.0);
        let p = GlideParams {
            glide_ratio: 8.0,
            safety_buffer_m: 0.0,
            takeoff_initial_agl_m: 0.0,
            ..Default::default()
        };
        let res = flood(&dem, &launch, &p);
        // Ideal radius: 1000m * 8 / cell_size = 320 cells.
        // Bounded by grid (40 cells from center). With 8-connected discrete
        // approximation this should reach the entire 81x81 grid.
        assert_eq!(res.reachable_cells, 81 * 81 - 1);
        // Furthest point: corner. Cell distance ~ 40*sqrt(2) cells ~ 56.6 cells * 25m = ~1414 m.
        assert!(res.max_distance_m > 1400.0, "got {}", res.max_distance_m);
    }

    #[test]
    fn cliff_blocks_reachability() {
        // Build a DEM with a wall to the east of the launch.
        let dem = Dem::from_fn(21, 21, 25.0, LV95::new(0.0, 0.0), |_, c| {
            if c >= 12 { 800.0 } else { 0.0 }
        });
        let launch = synthetic_launch(10, 5, 500.0);
        let p = GlideParams {
            glide_ratio: 8.0,
            safety_buffer_m: 30.0,
            takeoff_initial_agl_m: 0.0,
            ..Default::default()
        };
        let res = flood(&dem, &launch, &p);

        // Cells just west of launch should be reachable.
        let west = dem.idx(10, 0);
        assert!(res.max_alt_msl[west].is_finite(), "expected west reachable");
        // Cell on top of the wall well east should NOT be reachable (terrain too high vs altitude lost).
        let east = dem.idx(10, 18);
        assert!(
            res.max_alt_msl[east].is_nan(),
            "east cell unexpectedly reachable: alt={}",
            res.max_alt_msl[east]
        );
    }

    #[test]
    fn tailwind_extends_reach_downwind() {
        let dem = flat_low_dem(81, 81);
        let launch = synthetic_launch(40, 40, 100.0);
        let still = GlideParams {
            glide_ratio: 8.0,
            safety_buffer_m: 0.0,
            takeoff_initial_agl_m: 0.0,
            ..Default::default()
        };
        let r_still = flood(&dem, &launch, &still);

        // Wind FROM south at 18 km/h (5 m/s) pushes glides toward NORTH.
        let with_wind = GlideParams {
            wind: Some(Wind::new(180.0, 18.0)),
            ..still
        };
        let r_wind = flood(&dem, &launch, &with_wind);

        assert!(
            r_wind.max_distance_m > r_still.max_distance_m,
            "tailwind reach {} should exceed still-air {}",
            r_wind.max_distance_m,
            r_still.max_distance_m
        );
        // Farthest cell with a south wind should be north of launch (lower row).
        let f = r_wind.farthest_cell.expect("reached something");
        assert!(
            f.row < launch.cell.row,
            "farthest should be north of launch, got {f:?}"
        );
    }

    #[test]
    fn headwind_stronger_than_airspeed_blocks_direction() {
        // 50 km/h (~14 m/s) wind, airspeed 10 m/s -> can't make headway upwind.
        let dem = flat_low_dem(81, 81);
        let launch = synthetic_launch(40, 40, 100.0);
        let p = GlideParams {
            glide_ratio: 8.0,
            safety_buffer_m: 0.0,
            takeoff_initial_agl_m: 0.0,
            wind: Some(Wind::new(0.0, 50.0)), // wind FROM north
            ..Default::default()
        };
        let r = flood(&dem, &launch, &p);

        // Cells directly north of launch (into the wind) should be unreachable.
        let north_idx = dem.idx(launch.cell.row - 5, launch.cell.col);
        assert!(
            r.max_alt_msl[north_idx].is_nan(),
            "north-of-launch cell unexpectedly reachable into 50 km/h headwind"
        );
        // South cells (downwind) should be reachable.
        let south_idx = dem.idx(launch.cell.row + 5, launch.cell.col);
        assert!(
            r.max_alt_msl[south_idx].is_finite(),
            "south-of-launch cell unreachable downwind"
        );
    }

    #[test]
    fn higher_glide_ratio_reaches_further() {
        // 101x101 grid at 25m -> ~50 cells radius from center.
        // alt=100, GR=5 -> 500m reach = 20 cells; GR=10 -> 1000m = 40 cells. Both fit.
        let dem = flat_low_dem(101, 101);
        let launch = synthetic_launch(50, 50, 100.0);
        let mut p = GlideParams {
            glide_ratio: 5.0,
            safety_buffer_m: 0.0,
            takeoff_initial_agl_m: 0.0,
            ..Default::default()
        };
        let r1 = flood(&dem, &launch, &p);
        p.glide_ratio = 10.0;
        let r2 = flood(&dem, &launch, &p);
        assert!(r2.reachable_cells > r1.reachable_cells);
        assert!(r2.max_distance_m > r1.max_distance_m);
    }
}
