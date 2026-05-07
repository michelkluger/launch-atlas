//! Hiking-trail network as a small graph for fast valley→summit routing.
//!
//! At api startup we:
//!   1. Build a graph from OSM polyline segments (one node per LV95 vertex,
//!      one edge per consecutive pair, cost = Tobler × SAC class multiplier
//!      using DEM-derived slope).
//!   2. Snap each lift / valley seed onto the nearest trail node within
//!      `SNAP_RADIUS_M`, keeping its initial cost.
//!   3. Dijkstra over the trail graph to get cost-from-valley per trail node.
//!   4. Project each reached trail node back to a DEM cell and feed it into
//!      `hike_field` as an additional seed.
//!
//! The grid Dijkstra then propagates outward with off-trail Tobler cost.

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

use hikefly_core::{CellIdx, Dem};
use hikefly_data::trails::TrailSegment;

pub struct TrailGraph {
    /// LV95 (e, n) of each node.
    pub node_pos: Vec<(f64, f64)>,
    /// (from_idx, to_idx, seconds_to_traverse).
    pub edges: Vec<(u32, u32, f32)>,
    /// Edge indices touching each node.
    pub adj: Vec<Vec<u32>>,
}

pub fn build(trails: &[TrailSegment], dem: &Dem) -> TrailGraph {
    let mut node_index: HashMap<(i64, i64), u32> = HashMap::new();
    let mut node_pos: Vec<(f64, f64)> = Vec::new();
    let mut edges: Vec<(u32, u32, f32)> = Vec::new();

    for trail in trails {
        let mult = trail.sac.cost_multiplier();
        if trail.points.len() < 2 {
            continue;
        }
        for win in trail.points.windows(2) {
            let (e1, n1) = (win[0][0], win[0][1]);
            let (e2, n2) = (win[1][0], win[1][1]);

            let k1 = (e1.round() as i64, n1.round() as i64);
            let k2 = (e2.round() as i64, n2.round() as i64);
            let i1 = *node_index.entry(k1).or_insert_with(|| {
                node_pos.push((e1, n1));
                (node_pos.len() - 1) as u32
            });
            let i2 = *node_index.entry(k2).or_insert_with(|| {
                node_pos.push((e2, n2));
                (node_pos.len() - 1) as u32
            });
            if i1 == i2 {
                continue;
            }

            let dx = e2 - e1;
            let dy = n2 - n1;
            let dist_m = (dx * dx + dy * dy).sqrt() as f32;
            if dist_m < 0.5 {
                continue;
            }
            let z1 = lv95_to_alt(dem, e1, n1).unwrap_or(0.0);
            let z2 = lv95_to_alt(dem, e2, n2).unwrap_or(0.0);
            let dz = z2 - z1;
            // Schweizer Wanderwege / SAC standard hiking-time formula
            // (used on yellow/red signposts):
            //   4 km/h horizontal, 400 m/h ascent, 600 m/h descent
            //   total = max(h, v) + min(h, v) / 2
            let horiz_h = dist_m / 4000.0;
            let vert_h = if dz >= 0.0 { dz / 400.0 } else { -dz / 600.0 };
            let total_h = horiz_h.max(vert_h) + horiz_h.min(vert_h) * 0.5;
            let seconds = total_h * 3600.0 * mult;
            edges.push((i1, i2, seconds));
        }
    }

    let mut adj: Vec<Vec<u32>> = vec![Vec::new(); node_pos.len()];
    for (eid, &(a, b, _)) in edges.iter().enumerate() {
        adj[a as usize].push(eid as u32);
        adj[b as usize].push(eid as u32);
    }

    TrailGraph {
        node_pos,
        edges,
        adj,
    }
}

fn lv95_to_alt(dem: &Dem, e: f64, n: f64) -> Option<f32> {
    let cs = dem.cell_size_m as f64;
    let col = ((e - dem.origin.e) / cs + 0.5).floor() as i32;
    let row = ((dem.origin.n - n) / cs + 0.5).floor() as i32;
    if row < 0 || col < 0 || row >= dem.rows as i32 || col >= dem.cols as i32 {
        return None;
    }
    dem.get(row as u32, col as u32)
}

pub fn lv95_to_cell(dem: &Dem, e: f64, n: f64) -> Option<CellIdx> {
    let cs = dem.cell_size_m as f64;
    let col = ((e - dem.origin.e) / cs + 0.5).floor() as i32;
    let row = ((dem.origin.n - n) / cs + 0.5).floor() as i32;
    if row < 0 || col < 0 || row >= dem.rows as i32 || col >= dem.cols as i32 {
        return None;
    }
    Some(CellIdx::new(row as u32, col as u32))
}

pub fn cell_to_lv95(dem: &Dem, cell: CellIdx) -> (f64, f64) {
    let cs = dem.cell_size_m as f64;
    (
        dem.origin.e + cell.col as f64 * cs,
        dem.origin.n - cell.row as f64 * cs,
    )
}

/// Naive O(N) nearest-node search; called only at startup, ~50 seeds × 260k
/// nodes is trivial. If trail count grows >1M, swap for a kd-tree.
pub fn nearest_node(graph: &TrailGraph, e: f64, n: f64, max_radius_m: f32) -> Option<u32> {
    let r2 = (max_radius_m * max_radius_m) as f64;
    let mut best: Option<(u32, f64)> = None;
    for (idx, (ne, nn)) in graph.node_pos.iter().enumerate() {
        let dx = ne - e;
        let dy = nn - n;
        let d2 = dx * dx + dy * dy;
        if d2 > r2 {
            continue;
        }
        match best {
            Some((_, bd)) if d2 >= bd => {}
            _ => best = Some((idx as u32, d2)),
        }
    }
    best.map(|(i, _)| i)
}

#[derive(Eq, PartialEq)]
struct DjkNode {
    neg_cost_ms: i64,
    idx: u32,
}
impl Ord for DjkNode {
    fn cmp(&self, o: &Self) -> Ordering {
        self.neg_cost_ms.cmp(&o.neg_cost_ms)
    }
}
impl PartialOrd for DjkNode {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// Multi-source Dijkstra over the trail graph.
pub fn dijkstra(graph: &TrailGraph, seeds: &[(u32, f32)]) -> Vec<f32> {
    let n = graph.node_pos.len();
    let mut cost = vec![f32::INFINITY; n];
    let mut heap = BinaryHeap::new();
    for &(idx, c) in seeds {
        if c < cost[idx as usize] {
            cost[idx as usize] = c;
            heap.push(DjkNode {
                neg_cost_ms: -((c * 1000.0) as i64),
                idx,
            });
        }
    }
    while let Some(DjkNode { neg_cost_ms, idx }) = heap.pop() {
        let cur = (-neg_cost_ms) as f32 / 1000.0;
        if cur > cost[idx as usize] + 1e-3 {
            continue;
        }
        for &eid in &graph.adj[idx as usize] {
            let (a, b, sec) = graph.edges[eid as usize];
            let other = if a == idx { b } else { a };
            let nc = cur + sec;
            if nc < cost[other as usize] {
                cost[other as usize] = nc;
                heap.push(DjkNode {
                    neg_cost_ms: -((nc * 1000.0) as i64),
                    idx: other,
                });
            }
        }
    }
    cost
}

/// Project the trail-Dijkstra costs back onto DEM cells, returning seeds
/// suitable for `hike_field`. Multiple trail nodes may map to the same DEM
/// cell; we keep the minimum cost.
pub fn project_to_dem_seeds(
    graph: &TrailGraph,
    dem: &Dem,
    trail_cost: &[f32],
) -> Vec<(CellIdx, f32)> {
    let mut by_cell: HashMap<u64, f32> = HashMap::new();
    for (idx, &c) in trail_cost.iter().enumerate() {
        if !c.is_finite() {
            continue;
        }
        let (e, n) = graph.node_pos[idx];
        if let Some(cell) = lv95_to_cell(dem, e, n) {
            let key = (cell.row as u64) << 32 | cell.col as u64;
            let entry = by_cell.entry(key).or_insert(f32::INFINITY);
            if c < *entry {
                *entry = c;
            }
        }
    }
    by_cell
        .into_iter()
        .map(|(k, c)| {
            let row = (k >> 32) as u32;
            let col = (k & 0xFFFFFFFF) as u32;
            (CellIdx::new(row, col), c)
        })
        .collect()
}
