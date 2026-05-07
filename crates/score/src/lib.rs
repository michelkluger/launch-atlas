//! Scoring & ranking.
//!
//! `score = glide_value / hike_cost^alpha * wind_factor`, with `wind_factor`
//! the cos(Δ) softened mismatch between user wind and launch aspect, and
//! Pareto helpers for the (hike, glide) frontier so the user can pick a
//! preference along it instead of arguing with a single scalar.

use hikefly_core::{Launch, Wind};

#[derive(Debug, Clone, Copy)]
pub enum GlideMetric {
    /// Maximum straight-line distance reachable from launch (meters).
    MaxDistanceM,
    /// Total reachable footprint area (m^2).
    AreaM2,
}

#[derive(Debug, Clone, Copy)]
pub struct ScoringParams {
    pub metric: GlideMetric,
    /// Exponent on hike cost. 1.0 = "minutes of flight per minute of hiking".
    pub alpha: f32,
    /// Half-width (deg) inside which the wind factor stays at full strength.
    pub wind_full_strength_deg: f32,
}

impl Default for ScoringParams {
    fn default() -> Self {
        Self {
            metric: GlideMetric::MaxDistanceM,
            alpha: 1.0,
            wind_full_strength_deg: 15.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GlideStats {
    pub max_distance_m: f32,
    pub area_m2: f64,
}

/// Wind factor in [0, 1]. Within `full_strength_deg` it's 1; outside it falls
/// off as cos(Δ - full_strength). Returns 0 once `Δ` exceeds 90° + full.
pub fn wind_factor(launch_aspect_deg: f32, wind: Wind, full_strength_deg: f32) -> f32 {
    let delta = hikefly_core::geom::angle_diff_deg(wind.from_deg, launch_aspect_deg).abs();
    if delta <= full_strength_deg {
        return 1.0;
    }
    let beyond = delta - full_strength_deg;
    if beyond >= 90.0 {
        return 0.0;
    }
    beyond.to_radians().cos().max(0.0)
}

#[derive(Debug, Clone)]
pub struct Scored<'a> {
    pub launch: &'a Launch,
    pub glide_value: f32,
    pub hike_seconds: f32,
    pub wind_factor: f32,
    pub score: f32,
}

pub fn score_launch<'a>(
    launch: &'a Launch,
    glide: &GlideStats,
    hike_seconds: f32,
    wind: Option<Wind>,
    p: &ScoringParams,
) -> Scored<'a> {
    let glide_value = match p.metric {
        GlideMetric::MaxDistanceM => glide.max_distance_m,
        GlideMetric::AreaM2 => glide.area_m2 as f32,
    };
    let wf = match wind {
        Some(w) => wind_factor(launch.aspect_deg, w, p.wind_full_strength_deg),
        None => 1.0,
    };
    let denom = (hike_seconds.max(1.0)).powf(p.alpha);
    let score = glide_value / denom * wf;
    Scored {
        launch,
        glide_value,
        hike_seconds,
        wind_factor: wf,
        score,
    }
}

/// Pareto frontier on (hike_seconds asc, glide_value desc): a launch is on the
/// frontier if no other launch is at least as cheap to hike AND at least as
/// good to glide, with one strictly better.
pub fn pareto_frontier<'a>(scored: &[Scored<'a>]) -> Vec<usize> {
    let n = scored.len();
    let mut on_front = Vec::new();
    for i in 0..n {
        let dominated = scored.iter().enumerate().any(|(j, b)| {
            if i == j {
                return false;
            }
            let a = &scored[i];
            let weakly_better = b.hike_seconds <= a.hike_seconds && b.glide_value >= a.glide_value;
            let strictly_better = b.hike_seconds < a.hike_seconds || b.glide_value > a.glide_value;
            weakly_better && strictly_better
        });
        if !dominated {
            on_front.push(i);
        }
    }
    on_front.sort_by(|&a, &b| {
        scored[a]
            .hike_seconds
            .partial_cmp(&scored[b].hike_seconds)
            .unwrap()
    });
    on_front
}

#[cfg(test)]
mod tests {
    use super::*;
    use hikefly_core::{CellIdx, LaunchKind};

    fn launch_aspect(aspect: f32) -> Launch {
        Launch {
            id: 0,
            cell: CellIdx::new(0, 0),
            altitude_m: 1000.0,
            aspect_deg: aspect,
            aspect_window_deg: (aspect - 45.0, aspect + 45.0),
            slope_deg: 30.0,
            kind: LaunchKind::AutoDiscovered,
            warnings: vec![],
        }
    }

    #[test]
    fn wind_factor_aligned_is_one() {
        let l = launch_aspect(180.0);
        let f = wind_factor(l.aspect_deg, Wind::new(180.0, 10.0), 15.0);
        assert!((f - 1.0).abs() < 1e-4);
    }

    #[test]
    fn wind_factor_opposite_is_zero() {
        let l = launch_aspect(180.0);
        let f = wind_factor(l.aspect_deg, Wind::new(0.0, 10.0), 15.0);
        assert!(f < 0.01, "got {f}");
    }

    #[test]
    fn wind_factor_45deg_off_is_partial() {
        let l = launch_aspect(180.0);
        let f = wind_factor(l.aspect_deg, Wind::new(135.0, 10.0), 15.0);
        // 45deg off, 15deg full strength, beyond=30deg -> cos(30°) ~ 0.866
        assert!(f > 0.7 && f < 0.95, "got {f}");
    }

    #[test]
    fn pareto_filters_dominated() {
        let l1 = launch_aspect(180.0);
        let l2 = launch_aspect(180.0);
        let l3 = launch_aspect(180.0);
        let s1 = Scored {
            launch: &l1,
            glide_value: 5000.0,
            hike_seconds: 3000.0,
            wind_factor: 1.0,
            score: 0.0,
        };
        let s2 = Scored {
            launch: &l2,
            glide_value: 8000.0,
            hike_seconds: 6000.0,
            wind_factor: 1.0,
            score: 0.0,
        };
        // s3 is dominated by s2: same hike, less glide.
        let s3 = Scored {
            launch: &l3,
            glide_value: 4000.0,
            hike_seconds: 6000.0,
            wind_factor: 1.0,
            score: 0.0,
        };
        let frontier = pareto_frontier(&[s1, s2, s3]);
        assert_eq!(frontier, vec![0, 1]);
    }
}
