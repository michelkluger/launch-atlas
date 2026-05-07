//! Minimal swisstopo STAC client: list SwissALTI3D tile assets in a bbox.

use serde::Deserialize;
use std::time::Duration;

use crate::{DataError, Result};

const STAC_ITEMS: &str =
    "https://data.geo.admin.ch/api/stac/v0.9/collections/ch.swisstopo.swissalti3d/items";

/// One DEM tile resolved from the STAC API.
#[derive(Debug, Clone)]
pub struct TileAsset {
    /// 1km LV95 east origin (SW corner) in km, e.g. 2627.
    pub e_km: u32,
    /// 1km LV95 north origin (SW corner) in km, e.g. 1175.
    pub n_km: u32,
    /// Year of the snapshot (e.g. 2024).
    pub year: u32,
    /// Ground sample distance in meters (0.5 or 2.0).
    pub gsd: f32,
    /// Direct GeoTIFF URL.
    pub href: String,
}

#[derive(Debug, Deserialize)]
struct StacFeatureCollection {
    features: Vec<StacFeature>,
    #[serde(default)]
    links: Vec<StacLink>,
}

#[derive(Debug, Deserialize)]
struct StacFeature {
    id: String,
    #[serde(default)]
    properties: serde_json::Value,
    assets: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct StacLink {
    rel: String,
    href: String,
}

/// List 2m DEM tiles for a WGS84 bounding box. Returns only the latest year
/// available for each (e_km, n_km) tile.
pub fn list_tiles_2m(
    lon_min: f64,
    lat_min: f64,
    lon_max: f64,
    lat_max: f64,
) -> Result<Vec<TileAsset>> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("hikefly/0.1")
        .build()?;

    let mut url = format!(
        "{STAC_ITEMS}?bbox={lon_min},{lat_min},{lon_max},{lat_max}&limit=100"
    );
    let mut all: Vec<TileAsset> = Vec::new();

    loop {
        let body: StacFeatureCollection = client.get(&url).send()?.error_for_status()?.json()?;
        for f in &body.features {
            if let Some(t) = parse_tile(&f.id, &f.assets)? {
                all.push(t);
            }
        }
        let next = body
            .links
            .iter()
            .find(|l| l.rel == "next")
            .map(|l| l.href.clone());
        match next {
            Some(n) => url = n,
            None => break,
        }
    }

    // Keep latest year per (e_km, n_km).
    all.sort_by_key(|t| (t.e_km, t.n_km, std::cmp::Reverse(t.year)));
    let mut latest: Vec<TileAsset> = Vec::new();
    for t in all {
        if !latest
            .iter()
            .any(|x| x.e_km == t.e_km && x.n_km == t.n_km)
        {
            latest.push(t);
        }
    }
    Ok(latest)
}

fn parse_tile(id: &str, assets: &serde_json::Value) -> Result<Option<TileAsset>> {
    // id like "swissalti3d_2019_2626-1171"
    let parts: Vec<&str> = id.split('_').collect();
    if parts.len() != 3 {
        return Ok(None);
    }
    let year: u32 = match parts[1].parse() {
        Ok(y) => y,
        Err(_) => return Ok(None),
    };
    let coord_parts: Vec<&str> = parts[2].split('-').collect();
    if coord_parts.len() != 2 {
        return Ok(None);
    }
    let e_km: u32 = match coord_parts[0].parse() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let n_km: u32 = match coord_parts[1].parse() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    // Find the 2m EPSG:2056 .xyz.zip asset (more decode-robust than the COG).
    let asset_obj = match assets.as_object() {
        Some(o) => o,
        None => return Err(DataError::MissingAsset(id.to_string())),
    };
    for (name, val) in asset_obj {
        if !name.ends_with(".xyz.zip") {
            continue;
        }
        if !name.contains("_2_2056_") {
            continue;
        }
        if let Some(href) = val.get("href").and_then(|h| h.as_str()) {
            return Ok(Some(TileAsset {
                e_km,
                n_km,
                year,
                gsd: 2.0,
                href: href.to_string(),
            }));
        }
    }
    Err(DataError::MissingAsset(id.to_string()))
}
