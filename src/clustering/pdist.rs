//! Pairwise great-circle distances (port of `blitzortung/clustering/pdist.pyx`).
//!
//! The Python Cython module computes the all-pairs distances between event
//! coordinates with a haversine-equivalent formula on the unit sphere with
//! `R = 6371` km.  Note that, for byte-for-byte parity with the Python
//! implementation, [`TO_RAD`] uses the *approximate* constant `3.1415/180`
//! (not `pi/180`) — the reference implementation does the same.

/// Degree-to-radian factor used by the Python `pdist.pyx`
/// (`3.1415 / 180.0`, deliberately not `std::f64::consts::PI / 180`).
// The Python reference approximates pi with 3.1415; the approximation is
// intentional so the distances match `pdist.pyx` byte-for-byte.
#[allow(clippy::approx_constant)]
pub const TO_RAD: f64 = 3.1415 / 180.0;

/// Earth radius in kilometres (`pdist.pyx`).
pub const EARTH_RADIUS_KM: f64 = 6371.0;

/// Great-circle distance in kilometres between two `(lambda, phi)` coordinate
/// pairs (`lambda` = longitude/x, `phi` = latitude/y).
///
/// Port of `pdist.distance` / `pdist.distance_`:
///
/// ```text
/// to_rad = 3.1415 / 180
/// delta_x = cos(phi_2) cos(lambda_2) - cos(phi_1) cos(lambda_1)
/// delta_y = cos(phi_2) sin(lambda_2) - cos(phi_1) sin(lambda_1)
/// delta_z = sin(phi_2) - sin(phi_1)
/// c       = sqrt(delta_x^2 + delta_y^2 + delta_z^2)
/// return 6371 * 2 * asin(c / 2)
/// ```
pub fn distance(lambda_1: f64, phi_1: f64, lambda_2: f64, phi_2: f64) -> f64 {
    let cos_lambda_1 = (TO_RAD * lambda_1).cos();
    let sin_lambda_1 = (TO_RAD * lambda_1).sin();
    let cos_phi_1 = (TO_RAD * phi_1).cos();
    let sin_phi_1 = (TO_RAD * phi_1).sin();
    let cos_lambda_2 = (TO_RAD * lambda_2).cos();
    let sin_lambda_2 = (TO_RAD * lambda_2).sin();
    let cos_phi_2 = (TO_RAD * phi_2).cos();
    let sin_phi_2 = (TO_RAD * phi_2).sin();

    let delta_x = cos_phi_2 * cos_lambda_2 - cos_phi_1 * cos_lambda_1;
    let delta_y = cos_phi_2 * sin_lambda_2 - cos_phi_1 * sin_lambda_1;
    let delta_z = sin_phi_2 - sin_phi_1;

    let c = (delta_x * delta_x + delta_y * delta_y + delta_z * delta_z).sqrt();

    EARTH_RADIUS_KM * 2.0 * (c / 2.0).asin()
}

/// All-pairs distances for `data` (a list of `(lambda, phi)` pairs), returned in
/// the same lower-triangular order as `scipy`/`fastcluster` `pdist`: the pairs
/// `(i, j)` for `i < j` in row-major order.
pub fn pdist(data: &[(f64, f64)]) -> Vec<f64> {
    let number_of_points = data.len();
    let mut distances = Vec::with_capacity(number_of_points * (number_of_points - 1) / 2);
    for i in 0..number_of_points.saturating_sub(1) {
        for j in (i + 1)..number_of_points {
            distances.push(distance(data[i].0, data[i].1, data[j].0, data[j].1));
        }
    }
    distances
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_matches_python_reference() {
        // tests/test_clustering.py::TestPdist.test_distance
        let d = distance(11.0, 51.0, 11.1, 51.1);
        assert!((d - 13.133_874_300_397_196).abs() < 1e-9, "distance = {d}");
    }

    #[test]
    fn distance_is_symmetric_and_zero_for_same_point() {
        assert_eq!(distance(11.0, 51.0, 11.0, 51.0), 0.0);
        let a = distance(7.0, 45.0, 13.0, 52.0);
        let b = distance(13.0, 52.0, 7.0, 45.0);
        assert!((a - b).abs() < 1e-12);
    }

    #[test]
    fn pdist_uses_lower_triangular_order() {
        let data = [(11.0, 51.0), (11.1, 51.1), (12.0, 52.0)];
        let distances = pdist(&data);
        assert_eq!(distances.len(), 3);
        assert!((distances[0] - distance(11.0, 51.0, 11.1, 51.1)).abs() < 1e-12);
        assert!((distances[1] - distance(11.0, 51.0, 12.0, 52.0)).abs() < 1e-12);
        assert!((distances[2] - distance(11.1, 51.1, 12.0, 52.0)).abs() < 1e-12);
    }
}
