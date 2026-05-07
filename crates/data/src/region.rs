//! Stitch swisstopo SwissALTI3D 2m tiles into one Region, decimate to a target
//! cell size, and persist to a compact binary file.
//!
//! The region uses `Dem` semantics: row 0 is northernmost, columns increase
//! east. Origin is the LV95 of cell (0, 0)'s center.

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

use hikefly_core::{Dem, LV95};
use serde::{Deserialize, Serialize};

use crate::lv95::lv95_to_wgs84;
use crate::stac::{TileAsset, list_tiles_2m};
use crate::tiff_reader;
use crate::{DataError, Result};

/// User-facing description of a fetched region.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Region {
    pub origin_e: f64, // LV95 of cell (0,0) center
    pub origin_n: f64,
    pub cell_size_m: f32,
    pub rows: u32,
    pub cols: u32,
    pub data: Vec<f32>, // row-major; NaN = nodata
    pub bbox_lv95: BBox,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BBox {
    pub e_min: f64,
    pub n_min: f64,
    pub e_max: f64,
    pub n_max: f64,
}

impl Region {
    pub fn into_dem(self) -> Dem {
        Dem {
            rows: self.rows,
            cols: self.cols,
            cell_size_m: self.cell_size_m,
            origin: LV95 {
                e: self.origin_e,
                n: self.origin_n,
            },
            data: self.data,
        }
    }

    /// Min/max altitude (ignoring NaN).
    pub fn altitude_range(&self) -> (f32, f32) {
        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;
        for &v in &self.data {
            if v.is_finite() {
                if v < min {
                    min = v;
                }
                if v > max {
                    max = v;
                }
            }
        }
        (min, max)
    }
}

/// Fetch tiles for many bboxes at once, deduplicating shared tiles. The
/// resulting Region's bounding hull contains all bboxes, with NaN-filled gaps
/// between them. Use this to assemble a "sparse coverage" DEM across a
/// canton without paying for the full canton-area download.
pub fn fetch_multi_region(
    named_bboxes: &[(String, BBox)],
    target_cell_m: f32,
    cache_dir: Option<&Path>,
) -> Result<Region> {
    if named_bboxes.is_empty() {
        return Err(DataError::BadFormat("no bboxes given".into()));
    }
    if target_cell_m < 2.0 {
        return Err(DataError::BadFormat(
            "target_cell_m < 2 m is below source resolution".into(),
        ));
    }

    let mut e_min = f64::INFINITY;
    let mut n_min = f64::INFINITY;
    let mut e_max = f64::NEG_INFINITY;
    let mut n_max = f64::NEG_INFINITY;
    for (_, b) in named_bboxes {
        e_min = e_min.min(b.e_min);
        n_min = n_min.min(b.n_min);
        e_max = e_max.max(b.e_max);
        n_max = n_max.max(b.n_max);
    }

    if let Some(d) = cache_dir {
        std::fs::create_dir_all(d)?;
    }

    let snap = |x: f64, c: f32| (x / c as f64).round() * c as f64;
    let e_min = snap(e_min, target_cell_m);
    let n_min = snap(n_min, target_cell_m);
    let e_max = snap(e_max, target_cell_m);
    let n_max = snap(n_max, target_cell_m);
    let cols = ((e_max - e_min) / target_cell_m as f64).round() as u32;
    let rows = ((n_max - n_min) / target_cell_m as f64).round() as u32;
    let mut data = vec![f32::NAN; (rows * cols) as usize];

    // Resolve tiles per bbox; deduplicate across bboxes.
    let mut seen: HashSet<(u32, u32)> = HashSet::new();
    let mut all_tiles: Vec<TileAsset> = Vec::new();
    for (name, b) in named_bboxes {
        let (lon_min, lat_min) = lv95_to_wgs84(b.e_min, b.n_min);
        let (lon_max, lat_max) = lv95_to_wgs84(b.e_max, b.n_max);
        let region_tiles = list_tiles_2m(lon_min, lat_min, lon_max, lat_max)?;
        eprintln!("STAC: {:>3} tiles for '{}'", region_tiles.len(), name);
        for t in region_tiles {
            if seen.insert((t.e_km, t.n_km)) {
                all_tiles.push(t);
            }
        }
    }
    eprintln!(
        "STAC: {} unique tiles across {} regions",
        all_tiles.len(),
        named_bboxes.len()
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("hikefly/0.1")
        .build()?;

    let total = all_tiles.len();
    for (i, tile) in all_tiles.iter().enumerate() {
        eprintln!(
            "  [{}/{}] tile {}-{} ({}m)",
            i + 1,
            total,
            tile.e_km,
            tile.n_km,
            tile.gsd as u32
        );
        let bytes = read_tile_bytes(&client, tile, cache_dir)?;
        let decoded = tiff_reader::decode(&bytes, tile.e_km, tile.n_km)?;
        composite_tile_into(
            tile,
            &decoded,
            (e_min, n_min, e_max, n_max),
            target_cell_m,
            rows,
            cols,
            &mut data,
        );
    }

    let origin_e = e_min + target_cell_m as f64 * 0.5;
    let origin_n = n_max - target_cell_m as f64 * 0.5;

    Ok(Region {
        origin_e,
        origin_n,
        cell_size_m: target_cell_m,
        rows,
        cols,
        data,
        bbox_lv95: BBox {
            e_min,
            n_min,
            e_max,
            n_max,
        },
    })
}

/// Fetch all 2m tiles overlapping the LV95 bbox, stitch and decimate to
/// `target_cell_m`, returning a Region. `cache_dir` if provided is used to
/// avoid re-downloading tiles between runs.
pub fn fetch_region(
    bbox: BBox,
    target_cell_m: f32,
    cache_dir: Option<&Path>,
) -> Result<Region> {
    if target_cell_m < 2.0 {
        return Err(DataError::BadFormat(
            "target_cell_m < 2 m is below source resolution".into(),
        ));
    }
    let (lon_min, lat_min) = lv95_to_wgs84(bbox.e_min, bbox.n_min);
    let (lon_max, lat_max) = lv95_to_wgs84(bbox.e_max, bbox.n_max);

    let tiles = list_tiles_2m(lon_min, lat_min, lon_max, lat_max)?;
    if tiles.is_empty() {
        return Err(DataError::BadFormat(format!(
            "no DEM tiles in bbox {bbox:?}"
        )));
    }
    eprintln!("STAC: {} tiles overlap bbox", tiles.len());

    if let Some(d) = cache_dir {
        std::fs::create_dir_all(d)?;
    }

    // Snap output bbox to multiples of target_cell_m for clean alignment.
    let snap = |x: f64, c: f32| (x / c as f64).round() * c as f64;
    let e_min = snap(bbox.e_min, target_cell_m);
    let n_min = snap(bbox.n_min, target_cell_m);
    let e_max = snap(bbox.e_max, target_cell_m);
    let n_max = snap(bbox.n_max, target_cell_m);
    let cols = ((e_max - e_min) / target_cell_m as f64).round() as u32;
    let rows = ((n_max - n_min) / target_cell_m as f64).round() as u32;
    let mut data = vec![f32::NAN; (rows * cols) as usize];

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("hikefly/0.1")
        .build()?;

    let total = tiles.len();
    for (i, tile) in tiles.iter().enumerate() {
        eprintln!(
            "  [{}/{}] tile {}-{} ({}m)",
            i + 1,
            total,
            tile.e_km,
            tile.n_km,
            tile.gsd as u32
        );
        let bytes = read_tile_bytes(&client, tile, cache_dir)?;
        let decoded = tiff_reader::decode(&bytes, tile.e_km, tile.n_km)?;
        composite_tile_into(
            tile,
            &decoded,
            (e_min, n_min, e_max, n_max),
            target_cell_m,
            rows,
            cols,
            &mut data,
        );
    }

    // Origin = LV95 of cell (0, 0)'s CENTER. Cell (0, 0) covers
    // east [e_min, e_min+cs), north [n_max-cs, n_max). So center is
    // (e_min + cs/2, n_max - cs/2).
    let origin_e = e_min + target_cell_m as f64 * 0.5;
    let origin_n = n_max - target_cell_m as f64 * 0.5;

    Ok(Region {
        origin_e,
        origin_n,
        cell_size_m: target_cell_m,
        rows,
        cols,
        data,
        bbox_lv95: BBox {
            e_min,
            n_min,
            e_max,
            n_max,
        },
    })
}

fn read_tile_bytes(
    client: &reqwest::blocking::Client,
    tile: &TileAsset,
    cache_dir: Option<&Path>,
) -> Result<Vec<u8>> {
    let cache_path = cache_dir.map(|d| {
        d.join(format!(
            "swissalti3d_{}_{}-{}_2m.xyz.zip",
            tile.year, tile.e_km, tile.n_km
        ))
    });
    if let Some(p) = &cache_path {
        if p.exists() {
            let mut buf = Vec::new();
            BufReader::new(File::open(p)?).read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    let bytes = client.get(&tile.href).send()?.error_for_status()?.bytes()?;
    if let Some(p) = &cache_path {
        BufWriter::new(File::create(p)?).write_all(&bytes)?;
    }
    Ok(bytes.to_vec())
}

/// For every cell of the output grid, sample (with simple averaging) from the
/// tile if the cell falls inside the tile's LV95 footprint.
fn composite_tile_into(
    tile: &TileAsset,
    decoded: &tiff_reader::DecodedTile,
    out_bbox: (f64, f64, f64, f64),
    target_cell_m: f32,
    rows: u32,
    cols: u32,
    out: &mut [f32],
) {
    let (e_min, n_min, e_max, n_max) = out_bbox;
    let tile_e_min = tile.e_km as f64 * 1000.0;
    let tile_e_max = tile_e_min + 1000.0;
    let tile_n_min = tile.n_km as f64 * 1000.0;
    let tile_n_max = tile_n_min + 1000.0;
    let tile_w = decoded.width as usize;
    let tile_h = decoded.height as usize;
    let pixel_size = 1000.0 / tile_w as f64; // expected 2 m for 500x500

    // Output cell (r, c) covers LV95:
    //   east  [e_min + c*cs, e_min + (c+1)*cs)
    //   north [n_max - (r+1)*cs, n_max - r*cs)
    let cs = target_cell_m as f64;

    let c_lo = (((tile_e_min - e_min) / cs).floor().max(0.0) as i64).max(0) as u32;
    let c_hi = (((tile_e_max - e_min) / cs).ceil() as i64).min(cols as i64) as u32;
    let r_lo = (((n_max - tile_n_max) / cs).floor().max(0.0) as i64).max(0) as u32;
    let r_hi = (((n_max - tile_n_min) / cs).ceil() as i64).min(rows as i64) as u32;

    for r in r_lo..r_hi {
        let cell_n_max = n_max - r as f64 * cs;
        let cell_n_min = cell_n_max - cs;
        for c in c_lo..c_hi {
            let cell_e_min = e_min + c as f64 * cs;
            let cell_e_max = cell_e_min + cs;
            // Intersect with tile.
            let isect_e_min = cell_e_min.max(tile_e_min);
            let isect_e_max = cell_e_max.min(tile_e_max);
            let isect_n_min = cell_n_min.max(tile_n_min);
            let isect_n_max = cell_n_max.min(tile_n_max);
            if isect_e_max <= isect_e_min || isect_n_max <= isect_n_min {
                continue;
            }
            // Map to pixel coords inside the tile. pixel (0,0) is the NW
            // corner; col increases east, row increases south.
            let px_col_lo =
                (((isect_e_min - tile_e_min) / pixel_size).floor() as i64).max(0) as usize;
            let px_col_hi = (((isect_e_max - tile_e_min) / pixel_size).ceil() as i64)
                .min(tile_w as i64) as usize;
            let px_row_lo =
                (((tile_n_max - isect_n_max) / pixel_size).floor() as i64).max(0) as usize;
            let px_row_hi = (((tile_n_max - isect_n_min) / pixel_size).ceil() as i64)
                .min(tile_h as i64) as usize;

            let mut sum = 0.0_f64;
            let mut n_pix = 0u32;
            for pr in px_row_lo..px_row_hi {
                let row_off = pr * tile_w;
                for pc in px_col_lo..px_col_hi {
                    let v = decoded.data[row_off + pc];
                    if v.is_finite() {
                        sum += v as f64;
                        n_pix += 1;
                    }
                }
            }
            if n_pix > 0 {
                let avg = (sum / n_pix as f64) as f32;
                let idx = (r * cols + c) as usize;
                let prev = out[idx];
                out[idx] = if prev.is_finite() {
                    // Average with whatever was there from a neighboring tile.
                    (prev + avg) * 0.5
                } else {
                    avg
                };
            }
        }
    }
}

// ----- binary persistence -----

const MAGIC: &[u8; 4] = b"HFLY";
const VERSION: u32 = 1;

pub fn save_region(region: &Region, path: &Path) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    w.write_all(MAGIC)?;
    w.write_all(&VERSION.to_le_bytes())?;
    w.write_all(&region.origin_e.to_le_bytes())?;
    w.write_all(&region.origin_n.to_le_bytes())?;
    w.write_all(&region.cell_size_m.to_le_bytes())?;
    w.write_all(&region.rows.to_le_bytes())?;
    w.write_all(&region.cols.to_le_bytes())?;
    w.write_all(&region.bbox_lv95.e_min.to_le_bytes())?;
    w.write_all(&region.bbox_lv95.n_min.to_le_bytes())?;
    w.write_all(&region.bbox_lv95.e_max.to_le_bytes())?;
    w.write_all(&region.bbox_lv95.n_max.to_le_bytes())?;
    for v in &region.data {
        w.write_all(&v.to_le_bytes())?;
    }
    w.flush()?;
    Ok(())
}

pub fn load_region(path: &Path) -> Result<Region> {
    let mut r = BufReader::new(File::open(path)?);
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(DataError::BadFormat("bad magic".into()));
    }
    let mut buf4 = [0u8; 4];
    let mut buf8 = [0u8; 8];
    r.read_exact(&mut buf4)?;
    let version = u32::from_le_bytes(buf4);
    if version != VERSION {
        return Err(DataError::BadFormat(format!("unsupported version {version}")));
    }
    r.read_exact(&mut buf8)?;
    let origin_e = f64::from_le_bytes(buf8);
    r.read_exact(&mut buf8)?;
    let origin_n = f64::from_le_bytes(buf8);
    r.read_exact(&mut buf4)?;
    let cell_size_m = f32::from_le_bytes(buf4);
    r.read_exact(&mut buf4)?;
    let rows = u32::from_le_bytes(buf4);
    r.read_exact(&mut buf4)?;
    let cols = u32::from_le_bytes(buf4);
    r.read_exact(&mut buf8)?;
    let e_min = f64::from_le_bytes(buf8);
    r.read_exact(&mut buf8)?;
    let n_min = f64::from_le_bytes(buf8);
    r.read_exact(&mut buf8)?;
    let e_max = f64::from_le_bytes(buf8);
    r.read_exact(&mut buf8)?;
    let n_max = f64::from_le_bytes(buf8);

    let n_cells = (rows as usize) * (cols as usize);
    let mut data = vec![0.0f32; n_cells];
    for slot in &mut data {
        r.read_exact(&mut buf4)?;
        *slot = f32::from_le_bytes(buf4);
    }

    Ok(Region {
        origin_e,
        origin_n,
        cell_size_m,
        rows,
        cols,
        data,
        bbox_lv95: BBox {
            e_min,
            n_min,
            e_max,
            n_max,
        },
    })
}
