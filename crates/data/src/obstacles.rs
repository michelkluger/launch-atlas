//! BAZL aviation-obstacle catalog (Luftfahrthindernis ≥ 25 m AGL, plus
//! anything ≥ 5 m around aerodromes). Includes overhead cables (CATENARY),
//! cranes, antennas, wind turbines, towers, chimneys — every low-altitude
//! flight hazard the federal authority tracks.
//!
//! Layer: `ch.bazl.luftfahrthindernis`. CATENARY come back as 3D LineStrings
//! (cable span endpoints with altitude), CRANE/etc. as 3D Points.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::lv95::lv95_to_wgs84;
use crate::{DataError, Result};

const IDENTIFY_URL: &str =
    "https://api3.geo.admin.ch/rest/services/api/MapServer/identify";
const LAYER: &str = "ch.bazl.luftfahrthindernis";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ObstacleKind {
    Catenary,
    Crane,
    Antenna,
    Tower,
    WindTurbine,
    Building,
    Chimney,
    Other,
}

impl ObstacleKind {
    pub fn from_bazl(s: &str) -> Self {
        match s {
            "CATENARY" => Self::Catenary,
            "CRANE" => Self::Crane,
            "ANTENNA" => Self::Antenna,
            "TOWER" => Self::Tower,
            "WIND_TURBINE" | "WINDTURBINE" => Self::WindTurbine,
            "BUILDING" => Self::Building,
            "CHIMNEY" => Self::Chimney,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "coords")]
pub enum ObstacleGeom {
    /// Single point [lat, lon, z_amsl_m].
    Point([f64; 3]),
    /// Cable span / linear obstacle as a sequence of [lat, lon, z_amsl_m].
    Line(Vec<[f64; 3]>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Obstacle {
    pub kind: ObstacleKind,
    pub registration: String,
    /// Top elevation in meters AMSL.
    pub top_amsl_m: f32,
    /// Max height above ground in meters.
    pub max_agl_m: f32,
    pub geom: ObstacleGeom,
}

/// LV95 bbox: (e_min, n_min, e_max, n_max).
pub fn fetch_obstacles(bbox: (f64, f64, f64, f64)) -> Result<Vec<Obstacle>> {
    let (e_min, n_min, e_max, n_max) = bbox;
    let geom = format!("{e_min},{n_min},{e_max},{n_max}");
    let url = format!(
        "{IDENTIFY_URL}?geometryType=esriGeometryEnvelope&geometry={geom}&sr=2056\
         &imageDisplay=500,500,96&mapExtent={geom}&tolerance=0\
         &layers=all:{LAYER}&returnGeometry=true&geometryFormat=geojson&limit=2000"
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("hikefly/0.1 (obstacles)")
        .build()?;
    let body: serde_json::Value =
        client.get(&url).send()?.error_for_status()?.json()?;

    let results = body
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out: Vec<Obstacle> = Vec::new();
    for r in results {
        let props = r.get("properties").cloned().unwrap_or(serde_json::Value::Null);
        let kind = ObstacleKind::from_bazl(
            props.get("obstacletype").and_then(|x| x.as_str()).unwrap_or(""),
        );
        let registration = props
            .get("registrationnumber")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let top_amsl_m = props
            .get("topelevationamsl")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0) as f32;
        let max_agl_m = props
            .get("maxheightagl")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0) as f32;

        let geom = match r.get("geometry") {
            Some(g) => g,
            None => continue,
        };
        let gtype = geom.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let coords = match geom.get("coordinates") {
            Some(c) => c,
            None => continue,
        };
        let parsed = match gtype {
            "Point" => parse_point(coords).map(ObstacleGeom::Point),
            "LineString" => parse_linestring(coords).map(ObstacleGeom::Line),
            "Polygon" => coords
                .as_array()
                .and_then(|rings| rings.first())
                .and_then(parse_linestring)
                .map(ObstacleGeom::Line),
            _ => None,
        };
        if let Some(g) = parsed {
            out.push(Obstacle {
                kind,
                registration,
                top_amsl_m,
                max_agl_m,
                geom: g,
            });
        }
    }
    Ok(out)
}

fn parse_point(c: &serde_json::Value) -> Option<[f64; 3]> {
    let arr = c.as_array()?;
    if arr.len() < 2 {
        return None;
    }
    let e = arr[0].as_f64()?;
    let n = arr[1].as_f64()?;
    let z = arr.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0);
    let (lon, lat) = lv95_to_wgs84(e, n);
    Some([lat, lon, z])
}

fn parse_linestring(c: &serde_json::Value) -> Option<Vec<[f64; 3]>> {
    let arr = c.as_array()?;
    let mut out: Vec<[f64; 3]> = Vec::with_capacity(arr.len());
    for p in arr {
        if let Some(pt) = parse_point(p) {
            out.push(pt);
        }
    }
    if out.len() < 2 {
        None
    } else {
        Some(out)
    }
}

pub fn save_obstacles(o: &[Obstacle], path: &std::path::Path) -> Result<()> {
    serde_json::to_writer(std::fs::File::create(path)?, o)?;
    Ok(())
}

pub fn load_obstacles(path: &std::path::Path) -> Result<Vec<Obstacle>> {
    let bytes = std::fs::read(path)?;
    let o: Vec<Obstacle> = serde_json::from_slice(&bytes)
        .map_err(|e| DataError::BadFormat(e.to_string()))?;
    Ok(o)
}
