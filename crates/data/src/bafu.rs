//! Swiss BAFU canonical protected-area data via swisstopo's API3 identify
//! endpoint. Authoritative source for Wildruhezonen, federal hunting reserves
//! and national parks — replaces best-effort OSM `boundary=protected_area`
//! tagging.
//!
//! Reference: https://api3.geo.admin.ch/services/sdiservices.html#identify

use std::time::Duration;

use serde::Deserialize;

use crate::lv95::lv95_to_wgs84;
use crate::warnings::{Severity, Warning, WarningKind};
use crate::{DataError, Result};

const IDENTIFY_URL: &str =
    "https://api3.geo.admin.ch/rest/services/api/MapServer/identify";

/// BAFU layer codes (use for the `layers=all:<bodId>` parameter).
pub const LAYER_WILDRUHEZONEN: &str = "ch.bafu.wrz-wildruhezonen_portal";
pub const LAYER_HUNTING_RESERVES: &str = "ch.bafu.bundesinventare-jagdbanngebiete";
pub const LAYER_NATIONAL_PARKS: &str = "ch.bafu.schutzgebiete-paerke_nationaler_bedeutung";
/// BAZL airspace control zones (CTR — around airports).
pub const LAYER_CTR: &str = "ch.bazl.luftraeume-kontrollzonen";
/// BAZL terminal manoeuvring areas (TMA — approach/departure).
pub const LAYER_TMA: &str = "ch.bazl.luftraeume-nahkontrollbezirke";

/// LV95 bbox: (e_min, n_min, e_max, n_max).
pub type BBoxLv95 = (f64, f64, f64, f64);

#[derive(Debug, Deserialize)]
struct IdentifyResponse {
    #[serde(default)]
    results: Vec<IdentifyFeature>,
}

#[derive(Debug, Deserialize)]
struct IdentifyFeature {
    #[serde(default)]
    geometry: Option<serde_json::Value>,
    #[serde(default)]
    attributes: serde_json::Value,
    #[serde(default)]
    properties: serde_json::Value,
}

fn classify(layer: &str) -> Option<(WarningKind, Severity)> {
    match layer {
        LAYER_WILDRUHEZONEN => Some((WarningKind::Wildruhezone, Severity::High)),
        LAYER_HUNTING_RESERVES => Some((WarningKind::HuntingReserve, Severity::Critical)),
        LAYER_NATIONAL_PARKS => Some((WarningKind::NationalPark, Severity::High)),
        // CTR is the absolute exclusion around an airport — penetrating it
        // unannounced means a near-miss with airline traffic.
        LAYER_CTR => Some((WarningKind::AirspaceCtr, Severity::Critical)),
        // TMAs typically have a high floor (3000–4000 m AMSL); penalty is
        // medium — tag the launch but don't auto-filter, since most never
        // reach the floor in practice.
        LAYER_TMA => Some((WarningKind::AirspaceTma, Severity::Medium)),
        _ => None,
    }
}

/// Read a string-ish field from either `attributes` or `properties` (the API
/// is inconsistent across layer kinds).
fn attr_string(props: &serde_json::Value, attrs: &serde_json::Value, key: &str) -> Option<String> {
    if let Some(v) = props.get(key).and_then(|x| x.as_str()) {
        return Some(v.to_string());
    }
    if let Some(v) = attrs.get(key).and_then(|x| x.as_str()) {
        return Some(v.to_string());
    }
    None
}

fn pick_name(props: &serde_json::Value, attrs: &serde_json::Value) -> String {
    // Airspace items: combine type + name + designator + altitude range so
    // the tooltip shows e.g. "CTR GRENCHEN (LSZG, AGL→4500 ft)".
    let typ = attr_string(props, attrs, "type");
    if matches!(typ.as_deref(), Some("CTR" | "TMA" | "TMZ" | "RMZ" | "FIZ")) {
        let name = attr_string(props, attrs, "name").unwrap_or_default();
        let dsg = attr_string(props, attrs, "designator").unwrap_or_default();
        let lo = props
            .get("loli_value")
            .or_else(|| attrs.get("loli_value"))
            .and_then(|v| v.as_f64());
        let hi = props
            .get("upli_value")
            .or_else(|| attrs.get("upli_value"))
            .and_then(|v| v.as_f64());
        let lo_t = attr_string(props, attrs, "loli_type").unwrap_or_default();
        match (lo, hi) {
            (Some(lo), Some(hi)) => return format!("{name} ({dsg}, {lo:.0}{lo_t}–{hi:.0})"),
            _ => return format!("{name} ({dsg})"),
        }
    }
    for k in &["wrz_name", "name", "bezeichnung", "objektname", "gebiet"] {
        if let Some(s) = attr_string(props, attrs, k) {
            if !s.is_empty() {
                return s;
            }
        }
    }
    "(unnamed)".to_string()
}

/// Fetch one BAFU layer for an LV95 bbox. Returns one Warning per polygon
/// ring; multipolygon members each become their own Warning (we drop hole
/// topology like the OSM path).
pub fn fetch_layer(layer: &str, bbox: BBoxLv95) -> Result<Vec<Warning>> {
    let (e_min, n_min, e_max, n_max) = bbox;
    let (kind, severity) = match classify(layer) {
        Some(t) => t,
        None => {
            return Err(DataError::BadFormat(format!(
                "unknown BAFU layer: {layer}"
            )));
        }
    };

    let geom = format!("{},{},{},{}", e_min, n_min, e_max, n_max);
    let url = format!(
        "{IDENTIFY_URL}?geometryType=esriGeometryEnvelope&geometry={geom}&sr=2056\
         &imageDisplay=500,500,96&mapExtent={geom}&tolerance=10\
         &layers=all:{layer}&returnGeometry=true&geometryFormat=geojson&limit=400"
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("hikefly/0.1 (bafu)")
        .build()?;
    let body: IdentifyResponse = client.get(&url).send()?.error_for_status()?.json()?;

    let mut out: Vec<Warning> = Vec::new();
    for feat in &body.results {
        let name = pick_name(&feat.properties, &feat.attributes);
        let geom = match &feat.geometry {
            Some(g) => g,
            None => continue,
        };
        let geom_type = geom.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match geom_type {
            "Polygon" => {
                let coords = match geom.get("coordinates").and_then(|v| v.as_array()) {
                    Some(c) => c,
                    None => continue,
                };
                if let Some(outer) = coords.first() {
                    if let Some(poly) = parse_ring(outer) {
                        out.push(Warning {
                            name: name.clone(),
                            kind: kind.clone(),
                            severity: severity.clone(),
                            polygon: poly,
                        });
                    }
                }
            }
            "MultiPolygon" => {
                let parts = match geom.get("coordinates").and_then(|v| v.as_array()) {
                    Some(c) => c,
                    None => continue,
                };
                for poly_array in parts {
                    let rings = match poly_array.as_array() {
                        Some(r) => r,
                        None => continue,
                    };
                    if let Some(outer) = rings.first() {
                        if let Some(poly) = parse_ring(outer) {
                            out.push(Warning {
                                name: name.clone(),
                                kind: kind.clone(),
                                severity: severity.clone(),
                                polygon: poly,
                            });
                        }
                    }
                }
            }
            _ => continue,
        }
    }
    Ok(out)
}

/// Parse a GeoJSON ring (LV95 coords) into a vector of [lat, lon] WGS84
/// pairs. Returns None on shape errors.
fn parse_ring(ring: &serde_json::Value) -> Option<Vec<[f64; 2]>> {
    let arr = ring.as_array()?;
    let mut out = Vec::with_capacity(arr.len());
    for pt in arr {
        let pa = pt.as_array()?;
        if pa.len() < 2 {
            return None;
        }
        let e = pa[0].as_f64()?;
        let n = pa[1].as_f64()?;
        let (lon, lat) = lv95_to_wgs84(e, n);
        out.push([lat, lon]);
    }
    if out.len() < 3 {
        return None;
    }
    Some(out)
}

/// Convenience: fetch all BAFU + BAZL warning layers in one go.
pub fn fetch_all(bbox: BBoxLv95) -> Result<Vec<Warning>> {
    let mut all = Vec::new();
    for layer in &[
        LAYER_WILDRUHEZONEN,
        LAYER_HUNTING_RESERVES,
        LAYER_NATIONAL_PARKS,
        LAYER_CTR,
        LAYER_TMA,
    ] {
        match fetch_layer(layer, bbox) {
            Ok(mut v) => {
                eprintln!("swisstopo {:>4} polygons from {}", v.len(), layer);
                all.append(&mut v);
            }
            Err(e) => eprintln!("layer {layer} failed: {e}"),
        }
    }
    Ok(all)
}
