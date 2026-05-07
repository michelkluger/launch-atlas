//! OSM hiking trails filtered by SAC scale (Swiss Alpine Club difficulty).
//!
//! `sac_scale=hiking`              → T1, yellow Wanderweg (skipped by default)
//! `sac_scale=mountain_hiking`     → T2, white-red-white Bergweg
//! `sac_scale=demanding_mountain_hiking` → T3, harder Bergweg
//! `sac_scale=alpine_hiking`       → T4, blue-white-blue Alpinwanderweg
//! `sac_scale=demanding_alpine_hiking` / `difficult_alpine_hiking` → T5/T6
//!
//! For paragliding takeoff approaches we typically want T2+ (skip the yellow
//! valley-floor Wanderwege). The `min_sac` filter selects the entry point.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::lv95::wgs84_to_lv95;
use crate::{DataError, Result};

const OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SacScale {
    /// T1 — yellow Wanderweg, valley floors / forest paths.
    T1Hiking,
    /// T2 — white-red-white Bergweg, mountain hiking.
    T2MountainHiking,
    /// T3 — demanding Bergweg.
    T3DemandingMountain,
    /// T4 — blue-white-blue Alpinwanderweg.
    T4AlpineHiking,
    /// T5 — demanding alpine.
    T5DemandingAlpine,
    /// T6 — difficult alpine, glaciated.
    T6DifficultAlpine,
}

impl SacScale {
    pub fn from_osm(s: &str) -> Option<Self> {
        match s {
            "hiking" => Some(Self::T1Hiking),
            "mountain_hiking" => Some(Self::T2MountainHiking),
            "demanding_mountain_hiking" => Some(Self::T3DemandingMountain),
            "alpine_hiking" => Some(Self::T4AlpineHiking),
            "demanding_alpine_hiking" => Some(Self::T5DemandingAlpine),
            "difficult_alpine_hiking" => Some(Self::T6DifficultAlpine),
            _ => None,
        }
    }

    /// Trail-class multiplier on the standard Wanderwege time formula. Held
    /// at 1.0 across classes — the formula is calibrated against Swiss
    /// signpost times which already account for typical pace on each class.
    /// Steeper-class scrambles add some real-world drag, but Schweizer
    /// Wanderwege and SAC don't publish a per-class multiplier, so we don't
    /// invent one. The class is still kept for filtering/colouring.
    pub fn cost_multiplier(&self) -> f32 {
        1.0
    }

    pub fn overpass_regex(min: SacScale) -> String {
        let all = [
            (Self::T1Hiking, "hiking"),
            (Self::T2MountainHiking, "mountain_hiking"),
            (Self::T3DemandingMountain, "demanding_mountain_hiking"),
            (Self::T4AlpineHiking, "alpine_hiking"),
            (Self::T5DemandingAlpine, "demanding_alpine_hiking"),
            (Self::T6DifficultAlpine, "difficult_alpine_hiking"),
        ];
        let parts: Vec<&str> = all
            .iter()
            .filter(|(s, _)| *s >= min)
            .map(|(_, name)| *name)
            .collect();
        format!("^({})$", parts.join("|"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrailSegment {
    pub sac: SacScale,
    /// Polyline as LV95 (e, n) points.
    pub points: Vec<[f64; 2]>,
}

#[derive(Debug, Deserialize)]
struct OverpassResponse {
    elements: Vec<OverpassElement>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum OverpassElement {
    #[serde(rename = "way")]
    Way {
        #[allow(dead_code)]
        id: u64,
        #[serde(default)]
        geometry: Vec<OverpassPoint>,
        #[serde(default)]
        tags: std::collections::HashMap<String, String>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct OverpassPoint {
    lat: f64,
    lon: f64,
}

/// Fetch all hiking ways with `sac_scale >= min_sac` in the WGS84 bbox.
pub fn fetch_trails(
    lon_min: f64,
    lat_min: f64,
    lon_max: f64,
    lat_max: f64,
    min_sac: SacScale,
) -> Result<Vec<TrailSegment>> {
    let regex = SacScale::overpass_regex(min_sac);
    let query = format!(
        r#"[out:json][timeout:240];
(
  way["highway"~"^(path|footway|track)$"]["sac_scale"~"{regex}"]({lat_min},{lon_min},{lat_max},{lon_max});
);
out geom;"#
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(360))
        .user_agent("hikefly/0.1 (trails)")
        .build()?;

    let body: OverpassResponse = client
        .post(OVERPASS_URL)
        .form(&[("data", query.as_str())])
        .send()?
        .error_for_status()?
        .json()?;

    let mut out: Vec<TrailSegment> = Vec::new();
    for el in body.elements {
        let OverpassElement::Way { geometry, tags, .. } = el else { continue };
        if geometry.len() < 2 {
            continue;
        }
        let sac = match tags
            .get("sac_scale")
            .and_then(|s| SacScale::from_osm(s.as_str()))
        {
            Some(s) => s,
            None => continue,
        };
        let points: Vec<[f64; 2]> = geometry
            .iter()
            .map(|p| {
                let (e, n) = wgs84_to_lv95(p.lon, p.lat);
                [e, n]
            })
            .collect();
        out.push(TrailSegment { sac, points });
    }

    Ok(out)
}

pub fn save_trails(trails: &[TrailSegment], path: &std::path::Path) -> Result<()> {
    let f = std::fs::File::create(path)?;
    serde_json::to_writer(f, trails)?;
    Ok(())
}

pub fn load_trails(path: &std::path::Path) -> Result<Vec<TrailSegment>> {
    let bytes = std::fs::read(path)?;
    let trails: Vec<TrailSegment> =
        serde_json::from_slice(&bytes).map_err(|e| DataError::BadFormat(e.to_string()))?;
    Ok(trails)
}
