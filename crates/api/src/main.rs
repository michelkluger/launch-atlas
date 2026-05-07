//! Hikefly local web server.
//!
//! On startup loads `region.bin` (produced by `hikefly-fetch`) if present,
//! otherwise synthesises a Gaussian-peak DEM. Then auto-discovers launches,
//! runs a glide flood per launch, computes a hike-time field, renders a
//! hillshade PNG, and serves the frontend + JSON re-ranking API.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use hikefly_core::{CellIdx, Dem, LV95, Launch, Wind};
use hikefly_data::obstacles::{Obstacle, load_obstacles};
use hikefly_data::trails::{TrailSegment, load_trails};
use hikefly_data::{
    Lift, Severity, Warning, load_lifts, load_region, load_warnings, lv95_to_wgs84,
    point_in_polygon,
};
use hikefly_glide::{FloodSummary, GlideParams, flood, flood_summary};
use hikefly_hike::{HikeField, HikeParams, hike_field};
use hikefly_launch::{DiscoveryParams, discover};
use hikefly_score::{GlideMetric, GlideStats, ScoringParams, score_launch};
use hikefly_terrain::{SlopeAspect, slope_aspect};

mod hillshade;
mod png_util;
mod trail_graph;

#[derive(Debug, Clone, Copy)]
enum DataSource {
    Synthetic,
    SwissALTI3D,
}

struct AppState {
    dem: Dem,
    launches: Vec<Launch>,
    /// Cache of FloodSummary per (glide_ratio_x10) bucket. Populated lazily on
    /// the first request for that bucket and reused thereafter. Per-cell flood
    /// grids are NOT stored — those are recomputed on-demand for the single
    /// launch that the user clicks (it's only ~10 ms in release mode).
    flood_cache: RwLock<HashMap<FloodKey, Arc<Vec<FloodSummary>>>>,
    hike_with_lifts: HikeField,
    hike_without_lifts: HikeField,
    hillshade_png: Vec<u8>,
    source: DataSource,
    bbox_lv95: (f64, f64, f64, f64),
    trailheads: Vec<CellIdx>,
    lifts: Vec<Lift>,
    warnings: Vec<Warning>,
    launch_warnings: Vec<Vec<u32>>,
    trails_json: Vec<u8>,
    obstacles: Vec<Obstacle>,
}

const DEFAULT_GR: f32 = 8.0;
type FloodKey = (i32, i32, i32); // (gr*2, wind_from / 10°, wind_kmh / 2)

/// Snap (gr, wind) onto coarse buckets so neighbouring slider values share a
/// cached result. Buckets: GR ±0.25, wind dir ±5°, wind speed ±1 km/h.
fn bucket_inputs(glide_ratio: f32, wind: Option<Wind>) -> (f32, Option<Wind>, FloodKey) {
    let gr_b = (glide_ratio.clamp(2.0, 20.0) * 2.0).round() / 2.0;
    let gr_k = (gr_b * 2.0).round() as i32;
    match wind {
        Some(w) if w.speed_kmh > 0.0 => {
            // Floor-based bucketing so the bucket boundaries are stable: any
            // pair of adjacent slider values within the same 2 km/h or 10°
            // window collapses to the same key.
            let dir_b = (w.from_deg.rem_euclid(360.0) / 10.0).floor() * 10.0;
            let kmh_b = (w.speed_kmh / 2.0).floor() * 2.0;
            let bucketed = Wind {
                from_deg: dir_b,
                speed_kmh: kmh_b,
            };
            let key = (gr_k, dir_b as i32, kmh_b as i32);
            (gr_b, Some(bucketed), key)
        }
        _ => (gr_b, None, (gr_k, -1, -1)),
    }
}

fn glide_params(glide_ratio: f32, wind: Option<Wind>) -> GlideParams {
    GlideParams {
        glide_ratio,
        wind,
        ..Default::default()
    }
}

/// Look up flood summaries for `(glide_ratio, wind)`, computing in parallel
/// if absent. Wind shapes the reachable footprint per direction (tailwind
/// extends, headwind shrinks, near-stall headwind blocks).
fn get_or_compute_floods(
    state: &AppState,
    glide_ratio: f32,
    wind: Option<Wind>,
) -> Arc<Vec<FloodSummary>> {
    let (gr_b, wind_b, k) = bucket_inputs(glide_ratio, wind);
    {
        let cache = state.flood_cache.read().unwrap();
        if let Some(arc) = cache.get(&k) {
            return Arc::clone(arc);
        }
    }
    let t0 = std::time::Instant::now();
    let p = glide_params(gr_b, wind_b);
    let summaries: Vec<FloodSummary> = state
        .launches
        .par_iter()
        .map(|l| flood_summary(&state.dem, l, &p))
        .collect();
    let elapsed = t0.elapsed().as_secs_f32();
    let arc = Arc::new(summaries);
    state
        .flood_cache
        .write()
        .unwrap()
        .insert(k, Arc::clone(&arc));
    eprintln!(
        "  flood cache miss for {k:?}: {} launches in {:.2}s",
        arc.len(),
        elapsed
    );
    arc
}

fn synthetic_alps_dem() -> Dem {
    let rows = 161u32;
    let cols = 161u32;
    let cell = 25.0_f32;
    let cr = rows as f32 / 2.0;
    let cc = cols as f32 / 2.0;
    let valley_alt = 800.0_f32;
    let peak_alt = 2400.0_f32;
    let sigma = 35.0_f32;
    Dem::from_fn(rows, cols, cell, LV95::new(2_600_000.0, 1_200_000.0), |r, c| {
        let dr = r as f32 - cr;
        let dc = c as f32 - cc;
        let d2 = dr * dr + dc * dc;
        let bump = (peak_alt - valley_alt) * (-d2 / (2.0 * sigma * sigma)).exp();
        let tilt = -(r as f32 - cr) * 0.5;
        valley_alt + bump + tilt
    })
}

fn dem_bbox_lv95(dem: &Dem) -> (f64, f64, f64, f64) {
    let cs = dem.cell_size_m as f64;
    // dem.origin is the LV95 of cell (0, 0) center. Cell (0,0) is the NW corner
    // of the raster — row index increases southward.
    let e_min = dem.origin.e - cs * 0.5;
    let n_max = dem.origin.n + cs * 0.5;
    let e_max = e_min + cs * dem.cols as f64;
    let n_min = n_max - cs * dem.rows as f64;
    (e_min, n_min, e_max, n_max)
}

/// Connected components of non-NaN cells (4-connected). For each component
/// of at least `min_cells` cells, return the lowest-altitude cell as a
/// trailhead. This way a multi-region DEM gets one trailhead per island.
fn pick_trailheads(dem: &Dem, min_cells: u32) -> Vec<CellIdx> {
    let n = dem.cell_count();
    let mut label = vec![u32::MAX; n];
    let mut comp_size: Vec<u32> = Vec::new();
    let mut comp_lowest: Vec<(f32, CellIdx)> = Vec::new();
    let mut next: u32 = 0;
    let mut stack: Vec<(u32, u32)> = Vec::new();

    for r in 0..dem.rows {
        for c in 0..dem.cols {
            let i = dem.idx(r, c);
            if !dem.data[i].is_finite() || label[i] != u32::MAX {
                continue;
            }
            let mut size = 0u32;
            let mut lowest = (f32::INFINITY, CellIdx::new(r, c));
            stack.push((r, c));
            label[i] = next;
            while let Some((ar, ac)) = stack.pop() {
                let ai = dem.idx(ar, ac);
                let z = dem.data[ai];
                size += 1;
                if z < lowest.0 {
                    lowest = (z, CellIdx::new(ar, ac));
                }
                for (dr, dc) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let nr = ar as i32 + dr;
                    let nc = ac as i32 + dc;
                    if !dem.in_bounds(nr, nc) {
                        continue;
                    }
                    let nr = nr as u32;
                    let nc = nc as u32;
                    let ni = dem.idx(nr, nc);
                    if !dem.data[ni].is_finite() || label[ni] != u32::MAX {
                        continue;
                    }
                    label[ni] = next;
                    stack.push((nr, nc));
                }
            }
            comp_size.push(size);
            comp_lowest.push(lowest);
            next += 1;
        }
    }

    comp_lowest
        .into_iter()
        .zip(comp_size.into_iter())
        .filter(|(_, sz)| *sz >= min_cells)
        .map(|((_, c), _)| c)
        .collect()
}

fn precompute() -> AppState {
    let region_path = std::env::var("HIKEFLY_REGION")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("region.bin"));
    let (dem, source) = if region_path.exists() {
        match load_region(&region_path) {
            Ok(r) => {
                eprintln!(
                    "loaded region: {}x{} @ {} m, LV95 bbox ({:.0},{:.0})-({:.0},{:.0})",
                    r.rows,
                    r.cols,
                    r.cell_size_m,
                    r.bbox_lv95.e_min,
                    r.bbox_lv95.n_min,
                    r.bbox_lv95.e_max,
                    r.bbox_lv95.n_max,
                );
                (r.into_dem(), DataSource::SwissALTI3D)
            }
            Err(e) => {
                eprintln!("warning: failed to load {region_path:?}: {e}; falling back to synthetic");
                (synthetic_alps_dem(), DataSource::Synthetic)
            }
        }
    } else {
        eprintln!("no region.bin found; using synthetic DEM");
        (synthetic_alps_dem(), DataSource::Synthetic)
    };

    let bbox_lv95 = dem_bbox_lv95(&dem);
    let SlopeAspect { slope_deg, aspect_deg } = slope_aspect(&dem);
    let hillshade_png = hillshade::render(&dem, &slope_deg, &aspect_deg);

    let dparams = match source {
        DataSource::Synthetic => DiscoveryParams {
            max_launches: 80,
            ..Default::default()
        },
        DataSource::SwissALTI3D => DiscoveryParams {
            max_launches: 600,
            max_roughness_m: 4.0,
            // 700 m minimum spacing — distinct launches sit on different
            // flanks of the same peak, no clusters within walking distance.
            dedupe_radius_m: 700.0,
            ..Default::default()
        },
    };
    let raw_launches = discover(&dem, &dparams);
    eprintln!("  discovered: {} candidate launches", raw_launches.len());

    let trailheads = pick_trailheads(&dem, 200);
    eprintln!("  valley trailheads: {}", trailheads.len());

    // Load lifts if available; each lift contributes two seeds (lower at
    // cost 0, upper at cost = travel time + boarding overhead).
    let lifts_path = std::env::var("HIKEFLY_LIFTS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("lifts.json"));
    let lifts: Vec<Lift> = if lifts_path.exists() {
        match load_lifts(&lifts_path) {
            Ok(l) => {
                eprintln!("  lifts: {} loaded from {}", l.len(), lifts_path.display());
                l
            }
            Err(e) => {
                eprintln!("  lifts: failed to load {}: {e}", lifts_path.display());
                vec![]
            }
        }
    } else {
        eprintln!("  lifts: {} not present (no fast access)", lifts_path.display());
        vec![]
    };

    let obstacles_path = std::env::var("HIKEFLY_OBSTACLES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("obstacles.json"));
    let obstacles: Vec<Obstacle> = if obstacles_path.exists() {
        match load_obstacles(&obstacles_path) {
            Ok(o) => {
                eprintln!("  obstacles: {} loaded (BAZL Luftfahrthindernis)", o.len());
                o
            }
            Err(e) => {
                eprintln!("  obstacles load failed: {e}");
                vec![]
            }
        }
    } else {
        eprintln!("  obstacles: not present");
        vec![]
    };

    let warnings_path = std::env::var("HIKEFLY_WARNINGS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("warnings.json"));
    let warnings: Vec<Warning> = if warnings_path.exists() {
        match load_warnings(&warnings_path) {
            Ok(w) => {
                eprintln!("  warnings: {} loaded", w.len());
                w
            }
            Err(e) => {
                eprintln!("  warnings: failed to load: {e}");
                vec![]
            }
        }
    } else {
        eprintln!("  warnings: not present");
        vec![]
    };

    let valley_seeds: Vec<(CellIdx, f32)> = trailheads.iter().map(|c| (*c, 0.0)).collect();

    let mut lift_seeds_only: Vec<(CellIdx, f32)> = Vec::new();
    for lift in &lifts {
        // Goods cables are hazards, not passenger transport — don't seed
        // hike_field with them.
        if !lift.kind.is_human_transport() {
            continue;
        }
        if let Some((lo_cell, hi_cell, lo_alt, hi_alt)) = lift_endpoints_to_cells(&dem, lift) {
            let (bottom, top) = if lo_alt <= hi_alt {
                (lo_cell, hi_cell)
            } else {
                (hi_cell, lo_cell)
            };
            lift_seeds_only.push((bottom, 0.0));
            lift_seeds_only.push((top, lift.travel_seconds));
        }
    }
    eprintln!(
        "  lift seeds: {} (from {} lifts)",
        lift_seeds_only.len(),
        lift_seeds_only.len() / 2
    );

    // Hiking-trail network. If trails.json is present, build a graph and
    // pre-route from valley/lift seeds, then re-seed the grid hike_field with
    // the per-trail-node times.
    let trails_path = std::env::var("HIKEFLY_TRAILS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("trails.json"));
    let trails: Vec<TrailSegment> = if trails_path.exists() {
        match load_trails(&trails_path) {
            Ok(t) => {
                eprintln!("  trails: {} segments loaded", t.len());
                t
            }
            Err(e) => {
                eprintln!("  trails load failed: {e}");
                vec![]
            }
        }
    } else {
        eprintln!("  trails: not present (off-lift hike is cross-slope)");
        vec![]
    };
    let trail_graph = trail_graph::build(&trails, &dem);
    if !trail_graph.node_pos.is_empty() {
        eprintln!(
            "  trail graph: {} nodes, {} edges",
            trail_graph.node_pos.len(),
            trail_graph.edges.len()
        );
    }

    // Pre-serialize trails to lat/lon JSON for /api/trails. Computed once.
    #[derive(Serialize)]
    struct TrailOut<'a> {
        sac: &'a str,
        points: Vec<[f64; 2]>,
    }
    let trail_outs: Vec<TrailOut> = trails
        .iter()
        .map(|t| TrailOut {
            sac: match t.sac {
                hikefly_data::trails::SacScale::T1Hiking => "T1",
                hikefly_data::trails::SacScale::T2MountainHiking => "T2",
                hikefly_data::trails::SacScale::T3DemandingMountain => "T3",
                hikefly_data::trails::SacScale::T4AlpineHiking => "T4",
                hikefly_data::trails::SacScale::T5DemandingAlpine => "T5",
                hikefly_data::trails::SacScale::T6DifficultAlpine => "T6",
            },
            points: t
                .points
                .iter()
                .map(|p| {
                    let (lon, lat) = lv95_to_wgs84(p[0], p[1]);
                    [lat, lon]
                })
                .collect(),
        })
        .collect();
    let trails_json = serde_json::to_vec(&trail_outs).unwrap_or_default();
    if !trails_json.is_empty() {
        eprintln!(
            "  trails JSON: {:.1} MB cached",
            trails_json.len() as f32 / 1_000_000.0
        );
    }
    drop(trail_outs);

    /// Trail-aware hike_field: Dijkstra on the trail graph from `seeds`,
    /// project each reached trail node onto the DEM, then run grid Dijkstra
    /// from (`seeds` ∪ trail-node-seeds).
    let snap_radius_m: f32 = 250.0;
    let trail_seeds_for = |grid_seeds: &[(CellIdx, f32)]| -> Vec<(CellIdx, f32)> {
        if trail_graph.node_pos.is_empty() {
            return Vec::new();
        }
        let mut graph_seeds: Vec<(u32, f32)> = Vec::new();
        for &(cell, init) in grid_seeds {
            let (e, n) = trail_graph::cell_to_lv95(&dem, cell);
            if let Some(idx) = trail_graph::nearest_node(&trail_graph, e, n, snap_radius_m) {
                graph_seeds.push((idx, init));
            }
        }
        let trail_cost = trail_graph::dijkstra(&trail_graph, &graph_seeds);
        trail_graph::project_to_dem_seeds(&trail_graph, &dem, &trail_cost)
    };

    let t_hike = std::time::Instant::now();
    let mut seeds_with: Vec<(CellIdx, f32)> = valley_seeds.clone();
    seeds_with.extend(lift_seeds_only.iter().copied());
    let trail_seeds_with = trail_seeds_for(&seeds_with);
    seeds_with.extend(trail_seeds_with.iter().copied());
    let hike_with_lifts = hike_field(&dem, &seeds_with, &HikeParams::default());

    let mut seeds_without: Vec<(CellIdx, f32)> = valley_seeds.clone();
    let trail_seeds_without = trail_seeds_for(&seeds_without);
    seeds_without.extend(trail_seeds_without.iter().copied());
    let hike_without_lifts = hike_field(&dem, &seeds_without, &HikeParams::default());
    eprintln!(
        "  hike fields (with + without lifts, trails={}/{} seeds): {:.2}s",
        trail_seeds_with.len(),
        trail_seeds_without.len(),
        t_hike.elapsed().as_secs_f32()
    );

    // Compute warnings on the RAW launch set, then drop any launch inside a
    // Critical or High polygon — those are no-fly takeoffs (hunting reserves
    // year-round, Wildruhezonen seasonally, airports). The filtered list is
    // what becomes `launches`.
    let t_warn = std::time::Instant::now();
    let raw_warnings: Vec<Vec<u32>> = raw_launches
        .par_iter()
        .map(|l| {
            let (e, n) = (
                dem.origin.e + l.cell.col as f64 * dem.cell_size_m as f64,
                dem.origin.n - l.cell.row as f64 * dem.cell_size_m as f64,
            );
            let (lon, lat) = lv95_to_wgs84(e, n);
            warnings
                .iter()
                .enumerate()
                .filter_map(|(i, w)| {
                    if point_in_polygon(lat, lon, &w.polygon) {
                        Some(i as u32)
                    } else {
                        None
                    }
                })
                .collect()
        })
        .collect();

    let mut launches: Vec<Launch> = Vec::with_capacity(raw_launches.len());
    let mut launch_warnings: Vec<Vec<u32>> = Vec::with_capacity(raw_launches.len());
    let mut filtered_count = 0u32;
    for (l, ws) in raw_launches.into_iter().zip(raw_warnings.into_iter()) {
        let no_fly = ws.iter().any(|&wi| {
            matches!(
                warnings[wi as usize].severity,
                Severity::Critical | Severity::High
            )
        });
        if no_fly {
            filtered_count += 1;
            continue;
        }
        launches.push(l);
        launch_warnings.push(ws);
    }
    eprintln!(
        "  filtered out {} no-fly launches ({:.2}s); {} remain",
        filtered_count,
        t_warn.elapsed().as_secs_f32(),
        launches.len()
    );

    // Precompute the default still-air flood summaries up front so the first
    // page load is instant. Other (GR, wind) combos are computed lazily.
    let p = glide_params(DEFAULT_GR, None);
    let t0 = std::time::Instant::now();
    let summaries: Vec<FloodSummary> = launches
        .par_iter()
        .map(|l| flood_summary(&dem, l, &p))
        .collect();
    eprintln!(
        "  glide flood (GR={DEFAULT_GR}, still air, {} launches): {:.2}s",
        summaries.len(),
        t0.elapsed().as_secs_f32()
    );
    let mut cache: HashMap<FloodKey, Arc<Vec<FloodSummary>>> = HashMap::new();
    let (_, _, default_key) = bucket_inputs(DEFAULT_GR, None);
    cache.insert(default_key, Arc::new(summaries));

    let advisory = launch_warnings.iter().filter(|v| !v.is_empty()).count();
    if advisory > 0 {
        eprintln!(
            "  remaining advisory warnings (Medium/Low): {} launches",
            advisory
        );
    }

    AppState {
        dem,
        launches,
        flood_cache: RwLock::new(cache),
        hike_with_lifts,
        hike_without_lifts,
        hillshade_png,
        source,
        bbox_lv95,
        trailheads,
        lifts,
        warnings,
        launch_warnings,
        trails_json,
        obstacles,
    }
}

async fn handle_trails(State(state): State<Arc<AppState>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        state.trails_json.clone(),
    )
        .into_response()
}

/// Project an LV95 point onto the DEM grid; None if outside bounds.
fn lv95_to_cell(dem: &Dem, e: f64, n: f64) -> Option<CellIdx> {
    let cs = dem.cell_size_m as f64;
    let col_f = (e - dem.origin.e) / cs + 0.5;
    let row_f = (dem.origin.n - n) / cs + 0.5;
    let row = row_f.floor() as i32;
    let col = col_f.floor() as i32;
    if row < 0 || col < 0 || row >= dem.rows as i32 || col >= dem.cols as i32 {
        return None;
    }
    Some(CellIdx::new(row as u32, col as u32))
}

/// Project an LV95 point and, if the projected cell is NaN (outside any
/// fetched peak region), spiral-search outward up to `max_radius_m` for the
/// nearest cell with finite terrain. Lets lift endpoints in valley areas not
/// yet covered by peak bboxes still anchor to a nearby DEM cell.
fn snap_to_finite_cell(dem: &Dem, e: f64, n: f64, max_radius_m: f32) -> Option<(CellIdx, f32)> {
    let init = lv95_to_cell(dem, e, n)?;
    if let Some(z) = dem.get(init.row, init.col) {
        return Some((init, z));
    }
    let max_step = (max_radius_m / dem.cell_size_m).ceil() as i32;
    for r in 1..=max_step {
        // Walk the perimeter of the r-radius square, look for a finite cell.
        for dr in -r..=r {
            for &dc in &[-r, r] {
                let nr = init.row as i32 + dr;
                let nc = init.col as i32 + dc;
                if !dem.in_bounds(nr, nc) {
                    continue;
                }
                if let Some(z) = dem.get(nr as u32, nc as u32) {
                    return Some((CellIdx::new(nr as u32, nc as u32), z));
                }
            }
        }
        for dc in -r..=r {
            for &dr in &[-r, r] {
                let nr = init.row as i32 + dr;
                let nc = init.col as i32 + dc;
                if !dem.in_bounds(nr, nc) {
                    continue;
                }
                if let Some(z) = dem.get(nr as u32, nc as u32) {
                    return Some((CellIdx::new(nr as u32, nc as u32), z));
                }
            }
        }
    }
    None
}

/// Map a lift's two LV95 endpoints onto DEM cells with snap-to-finite.
fn lift_endpoints_to_cells(
    dem: &Dem,
    lift: &Lift,
) -> Option<(CellIdx, CellIdx, f32, f32)> {
    const SNAP_RADIUS_M: f32 = 600.0;
    let (lo, lo_alt) = snap_to_finite_cell(dem, lift.lower.e, lift.lower.n, SNAP_RADIUS_M)?;
    let (hi, hi_alt) = snap_to_finite_cell(dem, lift.upper.e, lift.upper.n, SNAP_RADIUS_M)?;
    Some((lo, hi, lo_alt, hi_alt))
}

#[derive(Debug, Deserialize)]
struct ScoreQuery {
    #[serde(default = "default_wind_from")]
    wind_from: f32,
    #[serde(default = "default_wind_kmh")]
    wind_kmh: f32,
    #[serde(default = "default_alpha")]
    alpha: f32,
    #[serde(default = "default_metric")]
    metric: String,
    #[serde(default = "default_use_wind")]
    use_wind: u8,
    #[serde(default = "default_glide_ratio")]
    glide_ratio: f32,
    #[serde(default = "default_use_lifts")]
    use_lifts: u8,
}

#[derive(Debug, Deserialize)]
struct ReachQuery {
    #[serde(default = "default_glide_ratio")]
    glide_ratio: f32,
    #[serde(default = "default_wind_from")]
    wind_from: f32,
    #[serde(default = "default_wind_kmh")]
    wind_kmh: f32,
    #[serde(default = "default_use_wind")]
    use_wind: u8,
}

fn default_wind_from() -> f32 { 180.0 }
fn default_wind_kmh() -> f32 { 8.0 }
fn default_alpha() -> f32 { 1.0 }
fn default_metric() -> String { "distance".into() }
fn default_use_wind() -> u8 { 1 }
fn default_glide_ratio() -> f32 { DEFAULT_GR }
fn default_use_lifts() -> u8 { 1 }

#[derive(Debug, Serialize)]
struct LaunchOut {
    id: u32,
    row: u32,
    col: u32,
    /// LV95 east of cell center.
    e: f64,
    /// LV95 north of cell center.
    n: f64,
    /// WGS84 longitude of cell center.
    lon: f64,
    /// WGS84 latitude of cell center.
    lat: f64,
    alt_m: f32,
    aspect_deg: f32,
    slope_deg: f32,
    glide_value: f32,
    glide_distance_km: f32,
    glide_area_km2: f32,
    hike_minutes: f32,
    wind_factor: f32,
    score: f32,
    /// Lat/lon of the farthest-reached ground cell (None if unreachable).
    farthest_lat: Option<f64>,
    farthest_lon: Option<f64>,
    /// Severity tags of warning polygons this launch sits inside.
    warnings: Vec<LaunchWarningOut>,
}

#[derive(Debug, Serialize)]
struct LaunchWarningOut {
    name: String,
    kind: String,
    severity: String,
}

#[derive(Debug, Serialize)]
struct DemInfo {
    rows: u32,
    cols: u32,
    cell_size_m: f32,
    /// 'synthetic' or 'swissalti3d'.
    source: String,
    /// LV95 bbox of the rendered region.
    bbox_lv95: [f64; 4],
    /// WGS84 bbox of the rendered region (lon_min, lat_min, lon_max, lat_max).
    bbox_wgs84: [f64; 4],
    /// Approximate 4 corners in lat/lon for an image overlay (NW, NE, SE, SW).
    corners_wgs84: [[f64; 2]; 4],
    /// Auto-detected trailheads, one per non-NaN connected component.
    trailheads: Vec<TrailheadOut>,
    /// Cable cars / funiculars / cog railways available as fast access.
    lifts: Vec<LiftOut>,
    /// Restricted-area polygons (Wildruhezonen, nature reserves, airports).
    warnings: Vec<WarningOut>,
    /// BAZL aviation obstacles (cable spans, cranes, antennas, etc).
    obstacles: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct WarningOut {
    name: String,
    kind: String,
    severity: String,
    /// Polygon ring [[lat, lon], ...].
    polygon: Vec<[f64; 2]>,
}

#[derive(Debug, Serialize)]
struct LiftOut {
    name: String,
    kind: String,
    /// Lower endpoint (lat, lon).
    lower: [f64; 2],
    /// Upper endpoint (lat, lon).
    upper: [f64; 2],
    travel_minutes: f32,
}

#[derive(Debug, Serialize)]
struct TrailheadOut {
    row: u32,
    col: u32,
    lon: f64,
    lat: f64,
    alt_m: f32,
}

fn cell_lv95(dem: &Dem, row: u32, col: u32) -> (f64, f64) {
    let cs = dem.cell_size_m as f64;
    (
        dem.origin.e + col as f64 * cs,
        dem.origin.n - row as f64 * cs,
    )
}

async fn handle_launches(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ScoreQuery>,
) -> Response {
    let metric = if q.metric == "area" {
        GlideMetric::AreaM2
    } else {
        GlideMetric::MaxDistanceM
    };
    let sparams = ScoringParams {
        metric,
        alpha: q.alpha,
        wind_full_strength_deg: 15.0,
    };
    let wind = if q.use_wind != 0 {
        Some(Wind::new(q.wind_from, q.wind_kmh))
    } else {
        None
    };

    // Wind affects glide path only when the user has it on AND speed > 0.
    let glide_wind = if q.use_wind != 0 && q.wind_kmh > 0.5 {
        Some(Wind::new(q.wind_from, q.wind_kmh))
    } else {
        None
    };
    let summaries = get_or_compute_floods(&state, q.glide_ratio, glide_wind);
    let hike = if q.use_lifts != 0 {
        &state.hike_with_lifts
    } else {
        &state.hike_without_lifts
    };
    let mut out: Vec<LaunchOut> = state
        .launches
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let f = &summaries[i];
            let gs = GlideStats {
                max_distance_m: f.max_distance_m,
                area_m2: f.area_m2,
            };
            let h = hike.seconds[state.dem.idx(l.cell.row, l.cell.col)];
            let hike_seconds = if h.is_finite() { h } else { f32::INFINITY };
            let s = score_launch(l, &gs, hike_seconds, wind, &sparams);
            let (e, n) = cell_lv95(&state.dem, l.cell.row, l.cell.col);
            let (lon, lat) = lv95_to_wgs84(e, n);
            let (farthest_lat, farthest_lon) = match f.farthest_cell {
                Some(c) => {
                    let (fe, fn_) = cell_lv95(&state.dem, c.row, c.col);
                    let (flon, flat) = lv95_to_wgs84(fe, fn_);
                    (Some(flat), Some(flon))
                }
                None => (None, None),
            };
            LaunchOut {
                id: l.id,
                row: l.cell.row,
                col: l.cell.col,
                e,
                n,
                lon,
                lat,
                alt_m: l.altitude_m,
                aspect_deg: l.aspect_deg,
                slope_deg: l.slope_deg,
                glide_value: s.glide_value,
                glide_distance_km: f.max_distance_m / 1000.0,
                glide_area_km2: (f.area_m2 / 1_000_000.0) as f32,
                hike_minutes: hike_seconds / 60.0,
                wind_factor: s.wind_factor,
                score: if s.score.is_finite() { s.score } else { 0.0 },
                farthest_lat,
                farthest_lon,
                warnings: state.launch_warnings[i]
                    .iter()
                    .map(|&wi| {
                        let w = &state.warnings[wi as usize];
                        LaunchWarningOut {
                            name: w.name.clone(),
                            kind: format!("{:?}", w.kind),
                            severity: format!("{:?}", w.severity),
                        }
                    })
                    .collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    axum::Json(out).into_response()
}

async fn handle_dem_info(State(state): State<Arc<AppState>>) -> Response {
    let (e_min, n_min, e_max, n_max) = state.bbox_lv95;
    // Project all 4 corners separately for accurate overlay placement.
    let nw = lv95_to_wgs84(e_min, n_max);
    let ne = lv95_to_wgs84(e_max, n_max);
    let se = lv95_to_wgs84(e_max, n_min);
    let sw = lv95_to_wgs84(e_min, n_min);
    let lon_min = nw.0.min(ne.0).min(se.0).min(sw.0);
    let lon_max = nw.0.max(ne.0).max(se.0).max(sw.0);
    let lat_min = nw.1.min(ne.1).min(se.1).min(sw.1);
    let lat_max = nw.1.max(ne.1).max(se.1).max(sw.1);
    let trailheads = state
        .trailheads
        .iter()
        .map(|t| {
            let (te, tn) = cell_lv95(&state.dem, t.row, t.col);
            let (tlon, tlat) = lv95_to_wgs84(te, tn);
            TrailheadOut {
                row: t.row,
                col: t.col,
                lon: tlon,
                lat: tlat,
                alt_m: state.dem.get(t.row, t.col).unwrap_or(0.0),
            }
        })
        .collect();
    // Resolve each lift's lower/upper endpoint by terrain altitude, so the
    // UI can render bottom→top in a consistent direction.
    let lifts_out: Vec<LiftOut> = state
        .lifts
        .iter()
        .map(|l| {
            let (lo_lat, lo_lon, hi_lat, hi_lon) = match lift_endpoints_to_cells(&state.dem, l) {
                Some((_, _, lo_alt, hi_alt)) if lo_alt <= hi_alt => {
                    (l.lower.lat, l.lower.lon, l.upper.lat, l.upper.lon)
                }
                Some(_) => (l.upper.lat, l.upper.lon, l.lower.lat, l.lower.lon),
                None => (l.lower.lat, l.lower.lon, l.upper.lat, l.upper.lon),
            };
            LiftOut {
                name: l.name.clone(),
                kind: format!("{:?}", l.kind),
                lower: [lo_lat, lo_lon],
                upper: [hi_lat, hi_lon],
                travel_minutes: l.travel_seconds / 60.0,
            }
        })
        .collect();

    let info = DemInfo {
        rows: state.dem.rows,
        cols: state.dem.cols,
        cell_size_m: state.dem.cell_size_m,
        source: match state.source {
            DataSource::Synthetic => "synthetic".into(),
            DataSource::SwissALTI3D => "swissalti3d".into(),
        },
        bbox_lv95: [e_min, n_min, e_max, n_max],
        bbox_wgs84: [lon_min, lat_min, lon_max, lat_max],
        corners_wgs84: [
            [nw.1, nw.0],
            [ne.1, ne.0],
            [se.1, se.0],
            [sw.1, sw.0],
        ],
        trailheads,
        lifts: lifts_out,
        warnings: state
            .warnings
            .iter()
            .map(|w| WarningOut {
                name: w.name.clone(),
                kind: format!("{:?}", w.kind),
                severity: format!("{:?}", w.severity),
                polygon: w.polygon.clone(),
            })
            .collect(),
        obstacles: state
            .obstacles
            .iter()
            .map(|o| serde_json::to_value(o).unwrap_or(serde_json::Value::Null))
            .collect(),
    };
    axum::Json(info).into_response()
}

async fn handle_hillshade(State(state): State<Arc<AppState>>) -> Response {
    (
        [(header::CONTENT_TYPE, "image/png")],
        state.hillshade_png.clone(),
    )
        .into_response()
}

async fn handle_reachability(
    State(state): State<Arc<AppState>>,
    Path(id): Path<u32>,
    Query(q): Query<ReachQuery>,
) -> Response {
    let idx = match state.launches.iter().position(|l| l.id == id) {
        Some(i) => i,
        None => return (StatusCode::NOT_FOUND, "launch not found").into_response(),
    };
    // Compute the full per-cell flood at the requested GR + wind for this
    // one launch. ~5-15 ms in release on the Bern-Oberland DEM.
    let glide_wind = if q.use_wind != 0 && q.wind_kmh > 0.5 {
        Some(Wind::new(q.wind_from, q.wind_kmh))
    } else {
        None
    };
    // Reachability mask uses the same bucketed (gr, wind) as the launch
    // ranking, so the marker arrow & footprint match the table values.
    let (gr_b, wind_b, _) = bucket_inputs(q.glide_ratio, glide_wind);
    let p = glide_params(gr_b, wind_b);
    let f = flood(&state.dem, &state.launches[idx], &p);
    let png = png_util::reachability_mask_png(&state.dem, &f);
    (
        [(header::CONTENT_TYPE, "image/png")],
        png,
    )
        .into_response()
}

const INDEX_HTML: &str = include_str!("../static/index.html");

async fn handle_index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

#[tokio::main]
async fn main() {
    eprintln!("hikefly: precomputing...");
    let t0 = std::time::Instant::now();
    let state = Arc::new(precompute());
    eprintln!(
        "  ready in {:.2}s ({} launches, {}x{} DEM, source={:?})",
        t0.elapsed().as_secs_f32(),
        state.launches.len(),
        state.dem.rows,
        state.dem.cols,
        state.source,
    );

    let app = Router::new()
        .route("/", get(handle_index))
        .route("/api/dem-info", get(handle_dem_info))
        .route("/api/hillshade.png", get(handle_hillshade))
        .route("/api/launches", get(handle_launches))
        .route("/api/reachability/{id}", get(handle_reachability))
        .route("/api/trails", get(handle_trails))
        .layer(CorsLayer::permissive())
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));
    let listener = tokio::net::TcpListener::bind(addr).await.expect("bind");
    eprintln!("hikefly: listening on http://{addr}");
    axum::serve(listener, app).await.expect("serve");
}
