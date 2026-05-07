//! Hiking-cost estimation.
//!
//! v1 implements Tobler's hiking function (1993) on the DEM grid as a
//! Dijkstra from a trailhead. SAC trail-class multipliers and the SwissTLM3D
//! trail graph slot in later — the API surface is shaped to accept either a
//! grid-based or graph-based cost source.
//!
//! Tobler:  walk_speed_kmh = 6 * exp(-3.5 * |dh/dx + 0.05|)
//!
//! The +0.05 offset means the fastest walking is on a 5% downhill grade, and
//! both steep up and steep down slow you down. Speed is converted to
//! seconds-per-meter for edge-weight purposes.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use hikefly_core::{CellIdx, Dem};

#[derive(Debug, Clone, Copy)]
pub enum SacClass {
    /// T1: Hiking. Wide, well-marked trail.
    T1,
    /// T2: Mountain hiking.
    T2,
    /// T3: Demanding mountain hiking.
    T3,
    /// T4: Alpine hiking. Off-trail / scrambling.
    T4,
    /// T5+: Demanding alpine. Filtered out by default for paraglider approaches.
    T5,
}

impl SacClass {
    pub fn cost_multiplier(&self) -> f32 {
        match self {
            SacClass::T1 => 1.0,
            SacClass::T2 => 1.2,
            SacClass::T3 => 1.5,
            SacClass::T4 => 2.0,
            SacClass::T5 => 3.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HikeParams {
    /// Default SAC class to assume when no trail metadata is available
    /// (i.e., when running grid Dijkstra without a labeled graph).
    pub default_sac: SacClass,
    /// Stop expanding cells whose cumulative cost exceeds this many seconds.
    pub max_cost_seconds: f32,
}

impl Default for HikeParams {
    fn default() -> Self {
        Self {
            default_sac: SacClass::T2,
            max_cost_seconds: 6.0 * 3600.0, // 6 h hard cap
        }
    }
}

#[derive(Debug, Clone)]
pub struct HikeField {
    /// Per-cell cumulative hiking cost in seconds, NaN = unreached.
    pub seconds: Vec<f32>,
}

/// Tobler's hiking function: kilometers per hour.
pub fn tobler_speed_kmh(slope_pct: f32) -> f32 {
    6.0 * (-3.5_f32 * (slope_pct + 0.05).abs()).exp()
}

/// Edge cost from `a` to `b` in seconds, using Tobler over the DEM.
fn edge_seconds(dem: &Dem, a: CellIdx, b: CellIdx, mult: f32) -> Option<f32> {
    let za = dem.get(a.row, a.col)?;
    let zb = dem.get(b.row, b.col)?;
    let dist = dem.distance_m(a, b);
    let slope = (zb - za) / dist;
    let speed_kmh = tobler_speed_kmh(slope).max(0.05); // floor to avoid ∞
    let speed_ms = speed_kmh * 1000.0 / 3600.0;
    Some(dist / speed_ms * mult)
}

#[derive(Debug)]
struct Node {
    cost_ms: u64, // milliseconds, for integer ordering
    cell: CellIdx,
}
impl PartialEq for Node {
    fn eq(&self, other: &Self) -> bool {
        self.cost_ms == other.cost_ms
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
        // Min-heap: invert.
        other.cost_ms.cmp(&self.cost_ms)
    }
}

const NEIGHBORS: [(i32, i32); 8] = [
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, -1),
    (0, 1),
    (1, -1),
    (1, 0),
    (1, 1),
];

/// Compute a hiking-time field from one or more seeded cells via grid
/// Dijkstra using Tobler's function. Each seed has its own initial cost in
/// seconds: 0 for a ground-level trailhead, the lift travel time for a cable
/// car upper station, etc. The lowest cost wins where multiple seeds compete.
pub fn hike_field(dem: &Dem, seeds: &[(CellIdx, f32)], p: &HikeParams) -> HikeField {
    let n = dem.cell_count();
    let mut cost = vec![f32::NAN; n];
    let mut heap = BinaryHeap::new();
    let mult = p.default_sac.cost_multiplier();

    for &(t, init) in seeds {
        if !dem.in_bounds(t.row as i32, t.col as i32) {
            continue;
        }
        let i = dem.idx(t.row, t.col);
        if cost[i].is_finite() && cost[i] <= init {
            continue;
        }
        cost[i] = init;
        heap.push(Node {
            cost_ms: (init * 1000.0).max(0.0) as u64,
            cell: t,
        });
    }

    while let Some(Node { cost_ms, cell }) = heap.pop() {
        let cur = cost_ms as f32 / 1000.0;
        let ci = dem.idx(cell.row, cell.col);
        if cost[ci].is_finite() && cost[ci] + 1e-3 < cur {
            continue;
        }
        if cur > p.max_cost_seconds {
            continue;
        }

        for (dr, dc) in NEIGHBORS {
            let nr = cell.row as i32 + dr;
            let nc = cell.col as i32 + dc;
            if !dem.in_bounds(nr, nc) {
                continue;
            }
            let neighbor = CellIdx::new(nr as u32, nc as u32);
            let edge = match edge_seconds(dem, cell, neighbor, mult) {
                Some(e) => e,
                None => continue,
            };
            let new_cost = cur + edge;
            if new_cost > p.max_cost_seconds {
                continue;
            }
            let ni = dem.idx(neighbor.row, neighbor.col);
            let prev = cost[ni];
            if !prev.is_finite() || new_cost + 1e-3 < prev {
                cost[ni] = new_cost;
                heap.push(Node {
                    cost_ms: (new_cost * 1000.0) as u64,
                    cell: neighbor,
                });
            }
        }
    }

    HikeField { seconds: cost }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hikefly_core::LV95;

    #[test]
    fn tobler_peak_is_at_5_percent_downhill() {
        let down = tobler_speed_kmh(-0.05);
        let flat = tobler_speed_kmh(0.0);
        let up = tobler_speed_kmh(0.10);
        assert!(down > flat && down > up);
        assert!((down - 6.0).abs() < 1e-3);
    }

    #[test]
    fn flat_grid_hike_field_grows_with_distance() {
        let dem = Dem::from_fn(21, 21, 25.0, LV95::new(0.0, 0.0), |_, _| 1000.0);
        let f = hike_field(&dem, &[(CellIdx::new(10, 10), 0.0)], &HikeParams::default());
        let near = f.seconds[dem.idx(10, 11)];
        let far = f.seconds[dem.idx(10, 20)];
        assert!(near.is_finite() && far.is_finite());
        assert!(far > near);
    }

    #[test]
    fn climb_is_slower_than_descent() {
        let ramp = Dem::from_fn(21, 21, 25.0, LV95::new(0.0, 0.0), |r, _| {
            1000.0 + (20 - r as i32) as f32 * 25.0
        });
        let f = hike_field(&ramp, &[(CellIdx::new(20, 10), 0.0)], &HikeParams::default());
        let climb_cost = f.seconds[ramp.idx(0, 10)];

        let flat = Dem::from_fn(21, 21, 25.0, LV95::new(0.0, 0.0), |_, _| 1000.0);
        let f2 = hike_field(&flat, &[(CellIdx::new(20, 10), 0.0)], &HikeParams::default());
        let flat_cost = f2.seconds[flat.idx(0, 10)];

        assert!(
            climb_cost > flat_cost * 1.5,
            "climb={climb_cost}, flat={flat_cost}"
        );
    }

    #[test]
    fn seeded_initial_cost_propagates() {
        // Two seeds: one with 0s, one with 1000s. The expensive seed's
        // territory is smaller because the cheap seed's frontier reaches
        // shared cells with lower cost first.
        let dem = Dem::from_fn(21, 21, 25.0, LV95::new(0.0, 0.0), |_, _| 1000.0);
        let f = hike_field(
            &dem,
            &[(CellIdx::new(10, 0), 0.0), (CellIdx::new(10, 20), 1000.0)],
            &HikeParams::default(),
        );
        // Cell at col 10 (midpoint): cheap seed dominates because going 10
        // cells of flat ground from the cheap seed costs ~~ 250m / 6 km/h * Tx
        // = a few minutes, far less than 1000 s.
        let mid = f.seconds[dem.idx(10, 10)];
        assert!(mid < 500.0, "cheap-seed should dominate midpoint; got {mid}");
    }
}
