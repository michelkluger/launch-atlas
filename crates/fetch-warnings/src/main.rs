//! Pull canonical safety/legal warning polygons from BAFU via swisstopo
//! API3 (Wildruhezonen, federal hunting reserves, national parks).
//!
//! Also folds in OSM airports (`aeroway=aerodrome`) as a crude CTR proxy
//! since BAFU doesn't publish airspace.

use std::path::PathBuf;
use std::process::ExitCode;

use hikefly_data::obstacles;
use hikefly_data::{bafu, fetch_warnings, save_warnings, wgs84_to_lv95};

const DEFAULT_BBOX_WGS84: (f64, f64, f64, f64) = (7.0, 46.4, 8.5, 47.0);

fn parse_args() -> Result<(f64, f64, f64, f64, PathBuf, bool), String> {
    let mut bbox = DEFAULT_BBOX_WGS84;
    let mut out = PathBuf::from("warnings.json");
    // BAZL CTRs cover proper airspace around airports; OSM airport polygons
    // are now off by default since they're a strict subset / less precise.
    let mut include_airports = false;
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bbox" => {
                let v = iter.next().ok_or_else(|| "--bbox needs 4 numbers".to_string())?;
                let parts: Vec<&str> = v.split(',').collect();
                if parts.len() != 4 {
                    return Err("--bbox needs 4 numbers".into());
                }
                bbox = (
                    parts[0].parse().map_err(|e| format!("lon_min: {e}"))?,
                    parts[1].parse().map_err(|e| format!("lat_min: {e}"))?,
                    parts[2].parse().map_err(|e| format!("lon_max: {e}"))?,
                    parts[3].parse().map_err(|e| format!("lat_max: {e}"))?,
                );
            }
            "--out" => {
                out = iter.next().ok_or_else(|| "--out needs path".to_string())?.into();
            }
            "--no-airports" => {
                include_airports = false;
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: hikefly-fetch-warnings [--bbox lon_min,lat_min,lon_max,lat_max] \
                     [--out warnings.json] [--no-airports]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok((bbox.0, bbox.1, bbox.2, bbox.3, out, include_airports))
}

fn main() -> ExitCode {
    let (lon_min, lat_min, lon_max, lat_max, out, include_airports) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "fetching BAFU warnings in bbox WGS84 ({lon_min},{lat_min}) -> ({lon_max},{lat_max})"
    );

    // Convert WGS84 bbox to LV95 for the API3 call.
    let (e_sw, n_sw) = wgs84_to_lv95(lon_min, lat_min);
    let (e_ne, n_ne) = wgs84_to_lv95(lon_max, lat_max);
    let bbox_lv95 = (e_sw.min(e_ne), n_sw.min(n_ne), e_sw.max(e_ne), n_sw.max(n_ne));

    let mut all = match bafu::fetch_all(bbox_lv95) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("BAFU fetch failed: {e}");
            return ExitCode::FAILURE;
        }
    };

    if include_airports {
        eprintln!("fetching OSM airports (CTR proxy)");
        match fetch_warnings(lon_min, lat_min, lon_max, lat_max) {
            Ok(osm) => {
                let airports: Vec<_> = osm
                    .into_iter()
                    .filter(|w| matches!(w.kind, hikefly_data::WarningKind::Airport))
                    .collect();
                eprintln!("OSM {:>4} airport polygons", airports.len());
                all.extend(airports);
            }
            Err(e) => eprintln!("OSM airport fetch failed: {e}"),
        }
    }

    // Also pull BAZL obstacles (cables + cranes + antennas + ...). Saved
    // alongside warnings.json as obstacles.json — different geometry/schema.
    eprintln!("fetching BAZL obstacles");
    match obstacles::fetch_obstacles(bbox_lv95) {
        Ok(obs) => {
            let mut by: std::collections::BTreeMap<String, u32> =
                std::collections::BTreeMap::new();
            for o in &obs {
                *by.entry(format!("{:?}", o.kind)).or_default() += 1;
            }
            eprintln!("BAZL obstacles: {} total", obs.len());
            for (k, n) in &by {
                eprintln!("  {n:>4}  {k}");
            }
            let obs_path = out
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("obstacles.json");
            if let Err(e) = obstacles::save_obstacles(&obs, &obs_path) {
                eprintln!("obstacles save failed: {e}");
            } else {
                eprintln!("wrote {}", obs_path.display());
            }
        }
        Err(e) => eprintln!("obstacles fetch failed: {e}"),
    }

    eprintln!("total polygons: {}", all.len());
    let mut by: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for w in &all {
        *by.entry(format!("{:?}", w.kind)).or_default() += 1;
    }
    for (k, n) in &by {
        eprintln!("  {n:>4}  {k}");
    }
    if let Err(e) = save_warnings(&all, &out) {
        eprintln!("save failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("wrote {}", out.display());
    ExitCode::SUCCESS
}
