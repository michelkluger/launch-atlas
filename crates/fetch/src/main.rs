//! Fetch one or more Swiss DEM regions and save as a single binary.
//!
//! Usage:
//!   hikefly-fetch [--bbox-lv95 e_min,n_min,e_max,n_max]   # one region
//!                 [--preset bern-oberland]                 # multi-region preset
//!                 [--cell 25] [--out region.bin] [--cache-dir cache/]
//!
//! Default (no flags): single bbox over Niederhorn / Beatenberg.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use hikefly_data::{BBox, fetch_multi_region, fetch_region, save_region, wgs84_to_lv95};
use serde::Deserialize;

const DEFAULT_BBOX: (f64, f64, f64, f64) =
    (2_626_500.0, 1_171_500.0, 2_631_500.0, 1_176_500.0);

/// Famous Bernese Oberland & Berner Jura flying areas, each a 3 km x 3 km
/// bbox centered on the launch summit. Coordinates are LV95 (e, n) in meters.
const BERN_OBERLAND: &[(&str, [f64; 4])] = &[
    // Classic Lake Thun / Lake Brienz amphitheatre
    ("Niederhorn",          [2_626_270.0, 1_173_110.0, 2_629_270.0, 1_176_110.0]),
    ("Niesen",              [2_613_450.0, 1_166_390.0, 2_616_450.0, 1_169_390.0]),
    ("Stockhorn",           [2_601_350.0, 1_165_350.0, 2_604_350.0, 1_168_350.0]),
    ("Sigriswiler Rothorn", [2_616_700.0, 1_179_200.0, 2_619_700.0, 1_182_200.0]),
    ("Augstmatthorn",       [2_638_600.0, 1_177_800.0, 2_641_600.0, 1_180_800.0]),
    ("Brienzer Rothorn",    [2_648_060.0, 1_178_910.0, 2_651_060.0, 1_181_910.0]),
    // Jungfrau region
    ("Männlichen",          [2_638_700.0, 1_162_350.0, 2_641_700.0, 1_165_350.0]),
    ("Faulhorn / First",    [2_643_900.0, 1_165_700.0, 2_646_900.0, 1_168_700.0]),
    ("Schilthorn",          [2_630_700.0, 1_154_500.0, 2_633_700.0, 1_157_500.0]),
    ("Wetterhorn",          [2_653_000.0, 1_164_500.0, 2_656_000.0, 1_167_500.0]),
    // Adelboden / Lenk / Saanen
    ("Hahnenmoos",          [2_603_300.0, 1_146_300.0, 2_606_300.0, 1_149_300.0]),
    ("Wildhorn",            [2_596_500.0, 1_142_500.0, 2_599_500.0, 1_145_500.0]),
    ("Wiriehorn",           [2_605_600.0, 1_155_200.0, 2_608_600.0, 1_158_200.0]),
    ("Saanenmöser",         [2_586_500.0, 1_147_500.0, 2_589_500.0, 1_150_500.0]),
    // Bern surrounds & Jura
    ("Gantrisch",           [2_597_500.0, 1_180_000.0, 2_600_500.0, 1_183_000.0]),
    ("Chasseral",           [2_571_500.0, 1_218_000.0, 2_574_500.0, 1_221_000.0]),
];

enum Mode {
    Single(BBox),
    Preset(Vec<(String, BBox)>),
    Peaks(Vec<(String, BBox)>),
}

#[derive(Debug, Deserialize)]
struct PeakDef {
    name: String,
    lat: f64,
    lon: f64,
    #[serde(default)]
    #[allow(dead_code)]
    ele: f64,
}

fn load_peaks(path: &Path, half_size_m: f64) -> Result<Vec<(String, BBox)>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let peaks: Vec<PeakDef> = serde_json::from_slice(&bytes)
        .map_err(|e| format!("parse {}: {e}", path.display()))?;
    let mut named_bboxes = Vec::with_capacity(peaks.len());
    for p in &peaks {
        let (e, n) = wgs84_to_lv95(p.lon, p.lat);
        named_bboxes.push((
            p.name.clone(),
            BBox {
                e_min: e - half_size_m,
                n_min: n - half_size_m,
                e_max: e + half_size_m,
                n_max: n + half_size_m,
            },
        ));
    }
    Ok(named_bboxes)
}

struct Args {
    mode: Mode,
    cell: f32,
    out: PathBuf,
    cache_dir: PathBuf,
}

fn preset(name: &str) -> Result<Vec<(String, BBox)>, String> {
    match name {
        "bern-oberland" => Ok(BERN_OBERLAND
            .iter()
            .map(|(n, [e0, n0, e1, n1])| {
                (
                    (*n).to_string(),
                    BBox {
                        e_min: *e0,
                        n_min: *n0,
                        e_max: *e1,
                        n_max: *n1,
                    },
                )
            })
            .collect()),
        other => Err(format!("unknown preset: {other}; try 'bern-oberland'")),
    }
}

fn parse_args() -> Result<Args, String> {
    let mut single_bbox: Option<BBox> = None;
    let mut preset_named: Option<Vec<(String, BBox)>> = None;
    let mut peaks_named: Option<Vec<(String, BBox)>> = None;
    let mut cell: f32 = 25.0;
    let mut out = PathBuf::from("region.bin");
    let mut cache_dir = PathBuf::from("cache");
    let mut peak_box_m: f64 = 1500.0; // 1.5 km square per peak (default)

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--peaks" => {
                let v = iter.next().ok_or_else(|| "--peaks needs a path".to_string())?;
                peaks_named = Some(load_peaks(Path::new(&v), peak_box_m * 0.5)?);
            }
            "--peak-box" => {
                let v = iter.next().ok_or_else(|| "--peak-box needs meters".to_string())?;
                peak_box_m = v.parse().map_err(|e| format!("peak-box: {e}"))?;
                // If --peaks already loaded, redo bbox sizes by re-loading.
                // Simpler rule: --peak-box must come before --peaks.
            }
            "--bbox-lv95" => {
                let v = iter
                    .next()
                    .ok_or_else(|| "--bbox-lv95 needs e_min,n_min,e_max,n_max".to_string())?;
                let parts: Vec<&str> = v.split(',').collect();
                if parts.len() != 4 {
                    return Err("--bbox-lv95 needs 4 comma-separated numbers".to_string());
                }
                single_bbox = Some(BBox {
                    e_min: parts[0].parse().map_err(|e| format!("e_min: {e}"))?,
                    n_min: parts[1].parse().map_err(|e| format!("n_min: {e}"))?,
                    e_max: parts[2].parse().map_err(|e| format!("e_max: {e}"))?,
                    n_max: parts[3].parse().map_err(|e| format!("n_max: {e}"))?,
                });
            }
            "--preset" => {
                let v = iter.next().ok_or_else(|| "--preset needs a name".to_string())?;
                preset_named = Some(preset(&v)?);
            }
            "--cell" => {
                let v = iter.next().ok_or_else(|| "--cell needs value".to_string())?;
                cell = v.parse().map_err(|e| format!("cell: {e}"))?;
            }
            "--out" => {
                out = iter.next().ok_or_else(|| "--out needs value".to_string())?.into();
            }
            "--cache-dir" => {
                cache_dir = iter
                    .next()
                    .ok_or_else(|| "--cache-dir needs value".to_string())?
                    .into();
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: hikefly-fetch [--bbox-lv95 e,n,e,n] [--preset bern-oberland] \
                     [--cell <m>] [--out <path>] [--cache-dir <dir>]"
                );
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    let mode = match (preset_named, single_bbox, peaks_named) {
        (Some(p), None, None) => Mode::Preset(p),
        (None, Some(b), None) => Mode::Single(b),
        (None, None, Some(p)) => Mode::Peaks(p),
        (None, None, None) => Mode::Single(BBox {
            e_min: DEFAULT_BBOX.0,
            n_min: DEFAULT_BBOX.1,
            e_max: DEFAULT_BBOX.2,
            n_max: DEFAULT_BBOX.3,
        }),
        _ => return Err("--preset, --bbox-lv95 and --peaks are mutually exclusive".into()),
    };
    Ok(Args { mode, cell, out, cache_dir })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    let region = match args.mode {
        Mode::Single(b) => {
            eprintln!(
                "fetch: bbox LV95 ({:.0}, {:.0}) -> ({:.0}, {:.0}); cell {} m",
                b.e_min, b.n_min, b.e_max, b.n_max, args.cell
            );
            match fetch_region(b, args.cell, Some(&args.cache_dir)) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("fetch failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Mode::Preset(named_bboxes) => {
            eprintln!(
                "fetch: preset with {} regions, cell {} m",
                named_bboxes.len(),
                args.cell
            );
            for (name, b) in &named_bboxes {
                eprintln!(
                    "  - {:<22}  ({:.0}, {:.0}) -> ({:.0}, {:.0})",
                    name, b.e_min, b.n_min, b.e_max, b.n_max
                );
            }
            match fetch_multi_region(&named_bboxes, args.cell, Some(&args.cache_dir)) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("fetch failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
        Mode::Peaks(named_bboxes) => {
            eprintln!(
                "fetch: peaks dataset with {} peaks, cell {} m",
                named_bboxes.len(),
                args.cell
            );
            match fetch_multi_region(&named_bboxes, args.cell, Some(&args.cache_dir)) {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("fetch failed: {e}");
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    let (alt_min, alt_max) = region.altitude_range();
    eprintln!(
        "region: {}x{} cells @ {} m  (alt {:.0}..{:.0} m)",
        region.rows, region.cols, region.cell_size_m, alt_min, alt_max
    );
    if let Err(e) = save_region(&region, &args.out) {
        eprintln!("save failed: {e}");
        return ExitCode::FAILURE;
    }
    eprintln!("wrote {}", args.out.display());
    ExitCode::SUCCESS
}
