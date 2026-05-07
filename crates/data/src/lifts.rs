//! Cable cars, gondolas, chair lifts, funiculars and rack railways from OSM.
//!
//! Each lift becomes a hikefly "fast access" segment: lower-station seed at
//! cost 0, upper-station seed at cost `travel_seconds + boarding_overhead`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::lv95::wgs84_to_lv95;
use crate::{DataError, Result};

const OVERPASS_URL: &str = "https://overpass-api.de/api/interpreter";
/// 3 minutes flat overhead for buying a ticket / boarding / waiting.
const BOARDING_OVERHEAD_S: f32 = 180.0;

/// Endpoint of one lift, in WGS84 + LV95.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiftEndpoint {
    pub lat: f64,
    pub lon: f64,
    pub e: f64,
    pub n: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LiftKind {
    CableCar,
    Gondola,
    ChairLift,
    DragLift,
    Funicular,
    CogRailway,
    /// Material cable / Materialseilbahn — farmers' cargo lifts. NOT human
    /// transport, but a real paragliding-flight hazard.
    GoodsLift,
}

impl LiftKind {
    /// Typical along-cable / along-rail speed in m/s.
    pub fn speed_ms(&self) -> f32 {
        match self {
            LiftKind::CableCar => 8.0,
            LiftKind::Gondola => 5.0,
            LiftKind::ChairLift => 2.5,
            LiftKind::DragLift => 2.5,
            LiftKind::Funicular => 8.0,
            LiftKind::CogRailway => 7.0,
            LiftKind::GoodsLift => 1.0,
        }
    }

    /// Whether this lift is usable for human transport (and thus a valid
    /// hike-access seed for the api). Goods lifts are visible hazards, not
    /// passenger transport.
    pub fn is_human_transport(&self) -> bool {
        !matches!(self, LiftKind::GoodsLift)
    }

    fn from_osm(aerialway: Option<&str>, railway: Option<&str>, rack: Option<&str>) -> Option<Self> {
        if let Some(a) = aerialway {
            return match a {
                "cable_car" => Some(LiftKind::CableCar),
                "gondola" | "mixed_lift" => Some(LiftKind::Gondola),
                "chair_lift" => Some(LiftKind::ChairLift),
                // Material / cargo cables (Materialseilbahn): farmers, mountain
                // huts, military supply. Visible cable hazard, not for humans.
                "goods" | "zip_line" => Some(LiftKind::GoodsLift),
                // Drag lifts are ski-only — skip in summer/paragliding context.
                _ => None,
            };
        }
        if let Some(r) = railway {
            // Only treat *mountain* railways as fast access. Plain narrow_gauge
            // / rail include mainline SBB/BLS trains in valleys.
            if r == "funicular" {
                return Some(LiftKind::Funicular);
            }
            if r == "rack_railway" {
                return Some(LiftKind::CogRailway);
            }
            // narrow_gauge/rail with `rack=*` set is a cog railway too.
            if (r == "narrow_gauge" || r == "rail") && rack.is_some() {
                return Some(LiftKind::CogRailway);
            }
        }
        None
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lift {
    pub name: String,
    pub kind: LiftKind,
    pub lower: LiftEndpoint,
    pub upper: LiftEndpoint,
    /// Horizontal distance in meters (Euclidean LV95).
    pub length_m: f32,
    /// Travel time in seconds, computed from kind speed + boarding overhead.
    pub travel_seconds: f32,
}

#[derive(Debug, Deserialize)]
struct OverpassResponse {
    elements: Vec<OverpassElement>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum OverpassElement {
    #[serde(rename = "node")]
    Node {
        id: u64,
        lat: f64,
        lon: f64,
    },
    #[serde(rename = "way")]
    Way {
        #[allow(dead_code)]
        id: u64,
        nodes: Vec<u64>,
        #[serde(default)]
        tags: std::collections::HashMap<String, String>,
    },
    #[serde(other)]
    Other,
}

/// Fetch all lifts whose geometry intersects the given WGS84 bbox.
pub fn fetch_lifts(
    lon_min: f64,
    lat_min: f64,
    lon_max: f64,
    lat_max: f64,
) -> Result<Vec<Lift>> {
    let query = format!(
        r#"[out:json][timeout:120];
(
  way["aerialway"~"^(cable_car|gondola|mixed_lift|chair_lift|goods|zip_line)$"]({lat_min},{lon_min},{lat_max},{lon_max});
  way["railway"="funicular"]({lat_min},{lon_min},{lat_max},{lon_max});
  way["railway"="rack_railway"]({lat_min},{lon_min},{lat_max},{lon_max});
  way["railway"~"^(narrow_gauge|rail)$"]["rack"]({lat_min},{lon_min},{lat_max},{lon_max});
);
out body;
>;
out skel;"#
    );

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(180))
        .user_agent("hikefly/0.1 (lifts)")
        .build()?;

    let body: OverpassResponse = client
        .post(OVERPASS_URL)
        .form(&[("data", query.as_str())])
        .send()?
        .error_for_status()?
        .json()?;

    let mut node_pos = std::collections::HashMap::new();
    struct WayInfo {
        nodes: Vec<u64>,
        kind: LiftKind,
        name: Option<String>,
    }
    let mut ways: Vec<WayInfo> = Vec::new();
    for el in body.elements {
        match el {
            OverpassElement::Node { id, lat, lon } => {
                node_pos.insert(id, (lat, lon));
            }
            OverpassElement::Way { nodes, tags, .. } => {
                if nodes.len() < 2 {
                    continue;
                }
                let kind = match LiftKind::from_osm(
                    tags.get("aerialway").map(|s| s.as_str()),
                    tags.get("railway").map(|s| s.as_str()),
                    tags.get("rack").map(|s| s.as_str()),
                ) {
                    Some(k) => k,
                    None => continue,
                };
                ways.push(WayInfo {
                    nodes,
                    kind,
                    name: tags.get("name").cloned(),
                });
            }
            _ => {}
        }
    }

    // Cluster ways into lift lines: ways sharing an endpoint node belong to the
    // same line. Cog railways and some funiculars are split across many short
    // OSM ways; without clustering each segment becomes a bogus "lift".
    let mut uf = UnionFind::new(ways.len());
    let mut node_to_ways: std::collections::HashMap<u64, Vec<usize>> =
        std::collections::HashMap::new();
    for (i, w) in ways.iter().enumerate() {
        let endpoints = [w.nodes[0], *w.nodes.last().unwrap()];
        for &n in &endpoints {
            node_to_ways.entry(n).or_default().push(i);
        }
    }
    for ws in node_to_ways.values() {
        for &wj in &ws[1..] {
            uf.union(ws[0], wj);
        }
    }
    let mut clusters: std::collections::HashMap<usize, Vec<usize>> =
        std::collections::HashMap::new();
    for i in 0..ways.len() {
        clusters.entry(uf.find(i)).or_default().push(i);
    }

    let mut lifts: Vec<Lift> = Vec::new();
    for (_root, way_idxs) in &clusters {
        // Collect kind frequency to pick the dominant kind. Take name from
        // the most common, or any non-empty fallback.
        let mut kind_counts: std::collections::HashMap<&str, u32> =
            std::collections::HashMap::new();
        for &wi in way_idxs {
            let key: &str = match ways[wi].kind {
                LiftKind::CableCar => "CableCar",
                LiftKind::Gondola => "Gondola",
                LiftKind::ChairLift => "ChairLift",
                LiftKind::DragLift => "DragLift",
                LiftKind::Funicular => "Funicular",
                LiftKind::CogRailway => "CogRailway",
                LiftKind::GoodsLift => "GoodsLift",
            };
            *kind_counts.entry(key).or_default() += 1;
        }
        let dominant = kind_counts
            .iter()
            .max_by_key(|(_, c)| *c)
            .map(|(k, _)| *k)
            .unwrap_or("Gondola");
        let kind = match dominant {
            "CableCar" => LiftKind::CableCar,
            "Gondola" => LiftKind::Gondola,
            "ChairLift" => LiftKind::ChairLift,
            "DragLift" => LiftKind::DragLift,
            "Funicular" => LiftKind::Funicular,
            "CogRailway" => LiftKind::CogRailway,
            "GoodsLift" => LiftKind::GoodsLift,
            _ => continue,
        };
        let name = way_idxs
            .iter()
            .filter_map(|&wi| ways[wi].name.clone())
            .next()
            .unwrap_or_else(|| format!("{:?}", kind));

        // Collect all distinct nodes in the cluster, compute their LV95.
        let mut points: Vec<(f64, f64, f64, f64)> = Vec::new(); // (lat, lon, e, n)
        let mut seen = std::collections::HashSet::new();
        for &wi in way_idxs {
            for &nid in &ways[wi].nodes {
                if !seen.insert(nid) {
                    continue;
                }
                let (lat, lon) = match node_pos.get(&nid) {
                    Some(p) => *p,
                    None => continue,
                };
                let (e, n) = wgs84_to_lv95(lon, lat);
                points.push((lat, lon, e, n));
            }
        }
        if points.len() < 2 {
            continue;
        }

        // Total along-track length: sum of all way segments.
        let mut total_length = 0.0_f64;
        for &wi in way_idxs {
            let w = &ways[wi];
            for win in w.nodes.windows(2) {
                let a = match node_pos.get(&win[0]) {
                    Some(p) => *p,
                    None => continue,
                };
                let b = match node_pos.get(&win[1]) {
                    Some(p) => *p,
                    None => continue,
                };
                let (ea, na) = wgs84_to_lv95(a.1, a.0);
                let (eb, nb) = wgs84_to_lv95(b.1, b.0);
                total_length += ((eb - ea).powi(2) + (nb - na).powi(2)).sqrt();
            }
        }

        // Two extreme endpoints = pair of points with maximum 2D distance.
        // O(n^2) but n is small per cluster.
        let mut best = (0usize, 0usize, 0.0_f64);
        for i in 0..points.len() {
            for j in (i + 1)..points.len() {
                let d = ((points[i].2 - points[j].2).powi(2)
                    + (points[i].3 - points[j].3).powi(2))
                .sqrt();
                if d > best.2 {
                    best = (i, j, d);
                }
            }
        }
        let p1 = &points[best.0];
        let p2 = &points[best.1];
        if total_length < 100.0 {
            continue; // junk
        }

        let travel = total_length as f32 / kind.speed_ms() + BOARDING_OVERHEAD_S;
        lifts.push(Lift {
            name,
            kind,
            lower: LiftEndpoint {
                lat: p1.0,
                lon: p1.1,
                e: p1.2,
                n: p1.3,
            },
            upper: LiftEndpoint {
                lat: p2.0,
                lon: p2.1,
                e: p2.2,
                n: p2.3,
            },
            length_m: total_length as f32,
            travel_seconds: travel,
        });
    }

    Ok(lifts)
}

struct UnionFind {
    parent: Vec<usize>,
    rank: Vec<u32>,
}
impl UnionFind {
    fn new(n: usize) -> Self {
        Self {
            parent: (0..n).collect(),
            rank: vec![0; n],
        }
    }
    fn find(&mut self, x: usize) -> usize {
        if self.parent[x] != x {
            let root = self.find(self.parent[x]);
            self.parent[x] = root;
        }
        self.parent[x]
    }
    fn union(&mut self, x: usize, y: usize) {
        let rx = self.find(x);
        let ry = self.find(y);
        if rx == ry {
            return;
        }
        if self.rank[rx] < self.rank[ry] {
            self.parent[rx] = ry;
        } else if self.rank[rx] > self.rank[ry] {
            self.parent[ry] = rx;
        } else {
            self.parent[ry] = rx;
            self.rank[rx] += 1;
        }
    }
}

pub fn save_lifts(lifts: &[Lift], path: &std::path::Path) -> Result<()> {
    let f = std::fs::File::create(path)?;
    serde_json::to_writer_pretty(f, lifts)?;
    Ok(())
}

pub fn load_lifts(path: &std::path::Path) -> Result<Vec<Lift>> {
    let bytes = std::fs::read(path)?;
    let lifts: Vec<Lift> =
        serde_json::from_slice(&bytes).map_err(|e| DataError::BadFormat(e.to_string()))?;
    Ok(lifts)
}
