//! Read a swisstopo SwissALTI3D 2m DEM tile from the `.xyz.zip` asset.
//!
//! The XYZ format is one space-separated triple per line:
//!   E_lv95 N_lv95 height_m
//! It's a CSV-like ASCII format much more forgiving than COG TIFF, at the
//! cost of being ~5x larger. Decompresses fine on any platform with no GDAL.
//!
//! Each tile is 500x500 points at 2m, with sample coordinates on cell
//! corners stepping by 2 m. We bin into a deterministic row-major grid
//! ordered north-first.

use std::io::{Cursor, Read};

use crate::Result;

const TILE_PIXEL_SIZE_M: f64 = 2.0;
const TILE_GRID_N: usize = 500;

/// Decoded DEM tile in row-major order, pixel (0, 0) at the NW corner.
pub struct DecodedTile {
    pub width: u32,
    pub height: u32,
    pub data: Vec<f32>,
}

/// `bytes` is the contents of a `.xyz.zip` asset. Returns the first XYZ entry.
pub fn decode(bytes: &[u8], tile_e_km: u32, tile_n_km: u32) -> Result<DecodedTile> {
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| crate::DataError::BadFormat(format!("zip: {e}")))?;
    let mut content = Vec::new();
    let mut found = false;
    for i in 0..zip.len() {
        let mut file = zip
            .by_index(i)
            .map_err(|e| crate::DataError::BadFormat(format!("zip entry: {e}")))?;
        if file.name().ends_with(".xyz") {
            file.read_to_end(&mut content)?;
            found = true;
            break;
        }
    }
    if !found {
        return Err(crate::DataError::BadFormat(
            "no .xyz file inside .xyz.zip".into(),
        ));
    }

    let tile_e_min = tile_e_km as f64 * 1000.0;
    let tile_n_max = (tile_n_km as f64 + 1.0) * 1000.0;

    let mut data = vec![f32::NAN; TILE_GRID_N * TILE_GRID_N];
    let s = std::str::from_utf8(&content)
        .map_err(|e| crate::DataError::BadFormat(format!("xyz utf8: {e}")))?;
    let mut count = 0u32;
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split_ascii_whitespace();
        let ex: f64 = match parts.next().and_then(|t| t.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let ny: f64 = match parts.next().and_then(|t| t.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        let z: f32 = match parts.next().and_then(|t| t.parse().ok()) {
            Some(v) => v,
            None => continue,
        };
        // Map (E, N) to (col, row) inside the 500x500 tile.
        let col_f = (ex - tile_e_min) / TILE_PIXEL_SIZE_M;
        let row_f = (tile_n_max - ny) / TILE_PIXEL_SIZE_M;
        let col = col_f.round() as i64;
        let row = row_f.round() as i64;
        if col < 0 || col >= TILE_GRID_N as i64 || row < 0 || row >= TILE_GRID_N as i64 {
            continue;
        }
        let idx = (row as usize) * TILE_GRID_N + (col as usize);
        data[idx] = z;
        count += 1;
    }
    if count == 0 {
        return Err(crate::DataError::BadFormat("xyz had no usable rows".into()));
    }

    Ok(DecodedTile {
        width: TILE_GRID_N as u32,
        height: TILE_GRID_N as u32,
        data,
    })
}
