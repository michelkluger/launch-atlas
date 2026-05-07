//! End-to-end smoke demo on a synthetic Swiss-style mountain.
//!
//! Pipeline:
//!   1. Build a synthetic DEM with a Gaussian peak ("Mt. Hikefly").
//!   2. Auto-discover candidate launches.
//!   3. Compute a hiking-time field from a "valley trailhead" via Tobler.
//!   4. Run the glide-flood per launch.
//!   5. Score with optional wind direction. Print top-N + Pareto frontier.

use hikefly_core::{CellIdx, Dem, LV95, Wind};
use hikefly_glide::{GlideParams, flood};
use hikefly_hike::{HikeParams, hike_field};
use hikefly_launch::{DiscoveryParams, discover};
use hikefly_score::{GlideMetric, GlideStats, ScoringParams, pareto_frontier, score_launch};

fn synthetic_alps_dem() -> Dem {
    // 161 x 161 cells at 25 m -> ~4 km square map.
    let rows = 161u32;
    let cols = 161u32;
    let cell = 25.0_f32;
    let cr = rows as f32 / 2.0;
    let cc = cols as f32 / 2.0;
    let valley_alt = 800.0_f32;
    let peak_alt = 2400.0_f32;
    let sigma = 35.0_f32; // cells

    Dem::from_fn(rows, cols, cell, LV95::new(2_600_000.0, 1_200_000.0), |r, c| {
        let dr = r as f32 - cr;
        let dc = c as f32 - cc;
        let d2 = dr * dr + dc * dc;
        let bump = (peak_alt - valley_alt) * (-d2 / (2.0 * sigma * sigma)).exp();
        // A subtle south-facing tilt so the south flank is slightly steeper -> better launches.
        let tilt = -(r as f32 - cr) * 0.5;
        valley_alt + bump + tilt
    })
}

fn main() {
    let dem = synthetic_alps_dem();
    println!(
        "DEM: {}x{} @ {} m/cell  ({:.1} km^2)",
        dem.rows,
        dem.cols,
        dem.cell_size_m,
        dem.area_m2() * (dem.rows * dem.cols) as f64 / 1_000_000.0
    );

    // 1. Auto-discover.
    let dparams = DiscoveryParams {
        max_launches: 80,
        ..Default::default()
    };
    let launches = discover(&dem, &dparams);
    println!("Auto-discovered {} candidate launches", launches.len());

    // 2. Hike-time field from a trailhead at the south edge of the map (valley floor).
    let trailhead = CellIdx::new(dem.rows - 2, dem.cols / 2);
    let hike = hike_field(&dem, &[trailhead], &HikeParams::default());

    // 3. Glide flood per launch (still air, GR 8.0).
    let gparams = GlideParams::default();

    // 4. Score against a south wind 8 km/h.
    let wind = Some(Wind::new(180.0, 8.0));
    let sparams = ScoringParams {
        metric: GlideMetric::MaxDistanceM,
        alpha: 1.0,
        wind_full_strength_deg: 15.0,
    };

    let mut scored = Vec::with_capacity(launches.len());
    let mut glide_per_launch: Vec<GlideStats> = Vec::with_capacity(launches.len());
    for l in &launches {
        let f = flood(&dem, l, &gparams);
        glide_per_launch.push(GlideStats {
            max_distance_m: f.max_distance_m,
            area_m2: f.area_m2,
        });
    }
    let mut hike_per_launch = Vec::with_capacity(launches.len());
    for l in &launches {
        let h = hike.seconds[dem.idx(l.cell.row, l.cell.col)];
        hike_per_launch.push(if h.is_finite() { h } else { f32::INFINITY });
    }
    for (i, l) in launches.iter().enumerate() {
        let s = score_launch(l, &glide_per_launch[i], hike_per_launch[i], wind, &sparams);
        scored.push(s);
    }
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap());

    println!();
    println!(
        "{:>3} {:>5} {:>5} {:>6} {:>7} {:>9} {:>7} {:>5} {:>7}",
        "#", "row", "col", "alt_m", "aspect", "glide_km", "hike_min", "wind", "m/min"
    );
    println!("{}", "-".repeat(70));
    for (rank, s) in scored.iter().take(10).enumerate() {
        // Score units: glide meters per hike-minute, scaled by wind factor.
        let m_per_min = s.score * 60.0;
        println!(
            "{:>3} {:>5} {:>5} {:>6.0} {:>7.0} {:>9.2} {:>7.0} {:>5.2} {:>7.1}",
            rank + 1,
            s.launch.cell.row,
            s.launch.cell.col,
            s.launch.altitude_m,
            s.launch.aspect_deg,
            s.glide_value / 1000.0,
            s.hike_seconds / 60.0,
            s.wind_factor,
            m_per_min
        );
    }

    println!();
    let frontier = pareto_frontier(&scored);
    println!("Pareto frontier ({} launches):", frontier.len());
    println!("  {:>8}  {:>9}  {:>7}", "hike_min", "glide_km", "m/min");
    for &i in &frontier {
        let s = &scored[i];
        let m_per_min = s.score * 60.0;
        println!(
            "  {:>8.0}  {:>9.2}  {:>7.1}",
            s.hike_seconds / 60.0,
            s.glide_value / 1000.0,
            m_per_min
        );
    }

    // Print best South-facing launch under the chosen wind, with explicit numbers.
    if let Some(best) = scored.first() {
        println!();
        println!(
            "Best for wind from {}\u{00b0}@{:.0} km/h: launch facing {:.0}\u{00b0} at \
             {:.0} m, slope {:.0}\u{00b0}; reachable {:.2} km, hike {:.0} min, \
             wind factor {:.2}",
            wind.unwrap().from_deg,
            wind.unwrap().speed_kmh,
            best.launch.aspect_deg,
            best.launch.altitude_m,
            best.launch.slope_deg,
            best.glide_value / 1000.0,
            best.hike_seconds / 60.0,
            best.wind_factor,
        );
    }
}
