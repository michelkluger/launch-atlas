//! Pull cable cars + funiculars + rack railways from OSM Overpass and save
//! to lifts.json. Used by `hikefly-api` to model fast valley→peak access.
//!
//! Default bbox covers Bernese Oberland + Berner Jura.

use std::path::PathBuf;
use std::process::ExitCode;

use hikefly_data::{fetch_lifts, save_lifts};

const DEFAULT_BBOX: (f64, f64, f64, f64) = (
    // (lon_min, lat_min, lon_max, lat_max)
    7.0, 46.4, 8.5, 47.0,
);

fn parse_args() -> Result<(f64, f64, f64, f64, PathBuf), String> {
    let mut bbox = DEFAULT_BBOX;
    let mut out = PathBuf::from("lifts.json");
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--bbox" => {
                let v = iter
                    .next()
                    .ok_or_else(|| "--bbox needs lon_min,lat_min,lon_max,lat_max".to_string())?;
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
            "-h" | "--help" => {
                eprintln!(
                    "usage: hikefly-fetch-trails [--bbox lon_min,lat_min,lon_max,lat_max] [--out lifts.json]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown arg: {other}")),
        }
    }
    Ok((bbox.0, bbox.1, bbox.2, bbox.3, out))
}

fn main() -> ExitCode {
    let (lon_min, lat_min, lon_max, lat_max, out) = match parse_args() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!(
        "fetching lifts in bbox ({lon_min},{lat_min}) -> ({lon_max},{lat_max})"
    );
    let lifts = match fetch_lifts(lon_min, lat_min, lon_max, lat_max) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("fetch failed: {e}");
            return ExitCode::FAILURE;
        }
    };
    eprintln!("got {} lifts", lifts.len());
    let mut by_kind: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for l in &lifts {
        *by_kind.entry(format!("{:?}", l.kind)).or_default() += 1;
    }
    for (k, n) in &by_kind {
        eprintln!("  {n:>4}  {k}");
    }
    if let Err(e) = save_lifts(&lifts, &out) {
        eprintln!("save failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("wrote {}", out.display());
    ExitCode::SUCCESS
}
