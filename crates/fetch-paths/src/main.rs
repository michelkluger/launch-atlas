//! Fetch OSM hiking trails (with SAC scale ≥ T2 by default) and save to
//! trails.json. Used by hikefly-api to model the off-lift hike via real
//! mountain paths instead of straight-line cross-slope.

use std::path::PathBuf;
use std::process::ExitCode;

use hikefly_data::trails::{SacScale, fetch_trails, save_trails};

const DEFAULT_BBOX: (f64, f64, f64, f64) = (7.0, 46.4, 8.5, 47.0);

fn parse_sac(s: &str) -> Result<SacScale, String> {
    Ok(match s {
        "T1" | "hiking" => SacScale::T1Hiking,
        "T2" | "mountain_hiking" => SacScale::T2MountainHiking,
        "T3" | "demanding_mountain_hiking" => SacScale::T3DemandingMountain,
        "T4" | "alpine_hiking" => SacScale::T4AlpineHiking,
        "T5" | "demanding_alpine_hiking" => SacScale::T5DemandingAlpine,
        "T6" | "difficult_alpine_hiking" => SacScale::T6DifficultAlpine,
        other => return Err(format!("unknown sac level: {other}")),
    })
}

fn parse_args() -> Result<(f64, f64, f64, f64, SacScale, PathBuf), String> {
    let mut bbox = DEFAULT_BBOX;
    let mut min_sac = SacScale::T2MountainHiking;
    let mut out = PathBuf::from("trails.json");
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
            "--min-sac" => {
                let v = iter.next().ok_or_else(|| "--min-sac needs T1..T6".to_string())?;
                min_sac = parse_sac(&v)?;
            }
            "--out" => {
                out = iter.next().ok_or_else(|| "--out needs path".to_string())?.into();
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: hikefly-fetch-paths [--bbox lon_min,lat_min,lon_max,lat_max] \
                     [--min-sac T1|T2|T3|T4|T5|T6] [--out trails.json]\n\
                     Default min-sac=T2 (skip yellow Wanderwege)"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok((bbox.0, bbox.1, bbox.2, bbox.3, min_sac, out))
}

fn main() -> ExitCode {
    let (lon_min, lat_min, lon_max, lat_max, min_sac, out) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "fetching OSM hiking trails (min_sac={:?}) in bbox ({lon_min},{lat_min}) -> ({lon_max},{lat_max})",
        min_sac
    );
    let trails = match fetch_trails(lon_min, lat_min, lon_max, lat_max, min_sac) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("fetch failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("got {} trail segments", trails.len());
    let mut by: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let mut total_pts = 0u64;
    for t in &trails {
        *by.entry(format!("{:?}", t.sac)).or_default() += 1;
        total_pts += t.points.len() as u64;
    }
    for (k, n) in &by {
        eprintln!("  {n:>5}  {k}");
    }
    eprintln!("total polyline vertices: {total_pts}");
    if let Err(e) = save_trails(&trails, &out) {
        eprintln!("save failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("wrote {}", out.display());
    ExitCode::SUCCESS
}
