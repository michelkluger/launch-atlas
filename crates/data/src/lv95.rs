//! Approximate LV95 ↔ WGS84 transforms (swisstopo simplified formulas).
//!
//! Accuracy is on the order of 1 meter, which is plenty for visualizing
//! launches on a public web map. For surveying-grade work you'd use PROJ.

/// Convert LV95 (E, N) in meters to WGS84 (lon, lat) in degrees.
pub fn lv95_to_wgs84(e: f64, n: f64) -> (f64, f64) {
    let e_aux = (e - 2_600_000.0) / 1_000_000.0;
    let n_aux = (n - 1_200_000.0) / 1_000_000.0;

    let lon = 2.6779094
        + 4.728982 * e_aux
        + 0.791484 * e_aux * n_aux
        + 0.1306 * e_aux * n_aux.powi(2)
        - 0.0436 * e_aux.powi(3);

    let lat = 16.9023892
        + 3.238272 * n_aux
        - 0.270978 * e_aux.powi(2)
        - 0.002528 * n_aux.powi(2)
        - 0.0447 * e_aux.powi(2) * n_aux
        - 0.0140 * n_aux.powi(3);

    (lon * 100.0 / 36.0, lat * 100.0 / 36.0)
}

/// Convert WGS84 (lon, lat) in degrees to LV95 (E, N) in meters.
pub fn wgs84_to_lv95(lon: f64, lat: f64) -> (f64, f64) {
    let phi_p = (lat * 3600.0 - 169_028.66) / 10_000.0;
    let lam_p = (lon * 3600.0 - 26_782.5) / 10_000.0;

    let e = 2_600_072.37
        + 211_455.93 * lam_p
        - 10_938.51 * lam_p * phi_p
        - 0.36 * lam_p * phi_p.powi(2)
        - 44.54 * lam_p.powi(3);

    let n = 1_200_147.07
        + 308_807.95 * phi_p
        + 3_745.25 * lam_p.powi(2)
        + 76.63 * phi_p.powi(2)
        - 194.56 * lam_p.powi(2) * phi_p
        + 119.79 * phi_p.powi(3);

    (e, n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn niederhorn_roundtrip() {
        // Niederhorn summit, approx LV95 (2627770, 1174610).
        // swisstopo's transform tool gives WGS84 (7.797°, 46.722°).
        let (e, n) = (2_627_770.0, 1_174_610.0);
        let (lon, lat) = lv95_to_wgs84(e, n);
        assert!((lon - 7.797).abs() < 0.01, "lon={lon}");
        assert!((lat - 46.722).abs() < 0.01, "lat={lat}");

        let (e2, n2) = wgs84_to_lv95(lon, lat);
        assert!((e2 - e).abs() < 2.0, "e roundtrip {e} -> {e2}");
        assert!((n2 - n).abs() < 2.0, "n roundtrip {n} -> {n2}");
    }
}
