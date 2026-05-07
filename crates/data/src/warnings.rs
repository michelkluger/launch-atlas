//! Safety / legal warning polygons from OSM Overpass.
//!
//! Currently covers:
//!   * Wildruhezonen (seasonal wildlife sanctuaries, OSM `boundary=protected_area`
//!     with name or protect_class hinting at "Wildruhezone")
//!   * Nature reserves (other `boundary=protected_area`)
//!   * Airports (`aeroway=aerodrome`) — crude CTR proxy until we plug
//!     skyguide / OpenAIP data
//!
//! Multi-polygon OSM relations are flattened to individual member-way
//! polygons; we lose hole topology but still cover the overlay accurately
//! enough for "is the launch inside any restricted area".

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{DataError, Result};

const OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum WarningKind {
    /// BAFU Wildruhezone — seasonal wildlife sanctuary.
    Wildruhezone,
    /// Federal hunting reserve (Eidg. Jagdbanngebiet) — year-round.
    HuntingReserve,
    /// Swiss National Park or park of national importance.
    NationalPark,
    /// Generic OSM `boundary=protected_area`.
    NatureReserve,
    /// Controlled airspace: CTR around an airport.
    AirspaceCtr,
    /// Controlled airspace: TMA / approach area.
    AirspaceTma,
    /// Airport / aerodrome (legacy).
    Airport,
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Warning {
    pub name: String,
    pub kind: WarningKind,
    pub severity: Severity,
    /// Closed polygon ring as [lat, lon] points. May not be self-closed; the
    /// point-in-polygon test treats first and last as connected.
    pub polygon: Vec<[f64; 2]>,
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
    #[serde(rename = "relation")]
    Relation {
        #[allow(dead_code)]
        id: u64,
        #[serde(default)]
        members: Vec<OverpassMember>,
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

#[derive(Debug, Deserialize)]
struct OverpassMember {
    #[serde(default)]
    role: String,
    #[serde(rename = "type")]
    member_type: String,
    #[serde(default)]
    geometry: Vec<OverpassPoint>,
}

pub fn fetch_warnings(
    lon_min: f64,
    lat_min: f64,
    lon_max: f64,
    lat_max: f64,
) -> Result<Vec<Warning>> {
    let query = format!(
        r#"[out:json][timeout:180];
(
  way["boundary"="protected_area"]({lat_min},{lon_min},{lat_max},{lon_max});
  relation["boundary"="protected_area"]({lat_min},{lon_min},{lat_max},{lon_max});
  way["leisure"="nature_reserve"]({lat_min},{lon_min},{lat_max},{lon_max});
  relation["leisure"="nature_reserve"]({lat_min},{lon_min},{lat_max},{lon_max});
  way["aeroway"="aerodrome"]({lat_min},{lon_min},{lat_max},{lon_max});
  relation["aeroway"="aerodrome"]({lat_min},{lon_min},{lat_max},{lon_max});
);
out geom;"#
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(240))
        .user_agent("hikefly/0.1 (warnings)")
        .build()?;

    let body: OverpassResponse = client
        .post(OVERPASS_URL)
        .form(&[("data", query.as_str())])
        .send()?
        .error_for_status()?
        .json()?;

    let mut out: Vec<Warning> = Vec::new();
    for el in body.elements {
        match el {
            OverpassElement::Way { geometry, tags, .. } => {
                if geometry.len() < 3 {
                    continue;
                }
                if let Some(w) = build_warning(&tags, &geometry) {
                    out.push(w);
                }
            }
            OverpassElement::Relation { members, tags, .. } => {
                // Flatten multipolygon: each "outer" member ring becomes one
                // Warning. Inner (hole) members are dropped.
                for m in members {
                    if m.member_type != "way" || m.geometry.len() < 3 {
                        continue;
                    }
                    if m.role == "inner" {
                        continue;
                    }
                    if let Some(w) = build_warning(&tags, &m.geometry) {
                        out.push(w);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(out)
}

fn build_warning(
    tags: &std::collections::HashMap<String, String>,
    geom: &[OverpassPoint],
) -> Option<Warning> {
    let aeroway = tags.get("aeroway").map(|s| s.as_str());
    let boundary = tags.get("boundary").map(|s| s.as_str());
    let leisure = tags.get("leisure").map(|s| s.as_str());
    let name = tags.get("name").cloned();
    let protect_class = tags.get("protect_class").map(|s| s.as_str());
    let aerodrome_type = tags.get("aerodrome:type").map(|s| s.as_str());

    let (kind, severity) = if aeroway == Some("aerodrome") {
        // Skip private grass strips and small unmarked fields.
        if let Some(t) = aerodrome_type {
            if !["public", "international", "regional", "military"].contains(&t) {
                return None;
            }
        } else if tags.get("iata").is_none() && tags.get("icao").is_none() {
            return None;
        }
        (WarningKind::Airport, Severity::Critical)
    } else if boundary == Some("protected_area") || leisure == Some("nature_reserve") {
        // Heuristic: name-contains "Wildruhe" or protect_class 4/24 (CH wildlife)
        let is_wrz = name
            .as_deref()
            .map(|n| n.to_lowercase().contains("wildruhe"))
            .unwrap_or(false)
            || matches!(protect_class, Some("4") | Some("24"));
        if is_wrz {
            (WarningKind::Wildruhezone, Severity::High)
        } else {
            (WarningKind::NatureReserve, Severity::Medium)
        }
    } else {
        (WarningKind::Other, Severity::Low)
    };

    let polygon: Vec<[f64; 2]> = geom.iter().map(|p| [p.lat, p.lon]).collect();
    Some(Warning {
        name: name.unwrap_or_else(|| format!("{:?}", kind)),
        kind,
        severity,
        polygon,
    })
}

pub fn save_warnings(ws: &[Warning], path: &std::path::Path) -> Result<()> {
    let f = std::fs::File::create(path)?;
    serde_json::to_writer(f, ws)?;
    Ok(())
}

pub fn load_warnings(path: &std::path::Path) -> Result<Vec<Warning>> {
    let bytes = std::fs::read(path)?;
    let ws: Vec<Warning> =
        serde_json::from_slice(&bytes).map_err(|e| DataError::BadFormat(e.to_string()))?;
    Ok(ws)
}

/// Ray-casting point-in-polygon. Polygon is a list of [lat, lon] points;
/// implicitly closed.
pub fn point_in_polygon(lat: f64, lon: f64, poly: &[[f64; 2]]) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = poly.len() - 1;
    for i in 0..poly.len() {
        let (xi, yi) = (poly[i][1], poly[i][0]);
        let (xj, yj) = (poly[j][1], poly[j][0]);
        if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}
