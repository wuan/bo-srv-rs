//! Geometry primitives for the strike grid service.
//!
//! Ported from `blitzortung/geom.py` (Envelope, Grid, GridFactory) and from
//! the `wuan/bo-server` JSON-RPC server (`grid` region table in
//! `scripts/bo-webservice.tac`).
//!
//! The UTM projection math is a port of PROJ's *Poder/Engsager* (etmerc)
//! implementation (`src/projections/tmerc.cpp` plus the auxiliary latitude
//! machinery in `src/latitudes.cpp`), which is the algorithm PROJ uses for
//! UTM zones by default.  The `utm` crate was tried and rejected because it
//! is neither accurate enough nor capable of southern hemisphere zones.

use std::f64::consts::PI;

const DEG_TO_RAD: f64 = PI / 180.0;

/// Band of supported UTM zones by region id (1..=7), exactly matching the
/// `grid` table in `blitzortung/gis/constants.py` on origin/main.
///
/// Tuple layout: `(region, min_lon, max_lon, min_lat, max_lat, utm_zone,
/// southern)`
pub const REGIONS: &[(u32, f64, f64, f64, f64, u32, bool)] = &[
    (1, -25.0, 57.0, 27.0, 72.0, 33, false),  // UTM 33N / WGS84 (Europe)
    (2, 110.0, 180.0, -50.0, 0.0, 55, true),  // UTM 55S / WGS84 (Oceania)
    (3, -140.0, -50.0, 10.0, 60.0, 14, false), // UTM 14N / WGS84 (North America)
    (4, 85.0, 150.0, -10.0, 60.0, 50, false), // UTM 50N / WGS84 (Asia)
    (5, -100.0, -30.0, -50.0, 20.0, 20, true), // UTM 20S / WGS84 (South America)
    (6, -20.0, 50.0, -40.0, 40.0, 33, false), // UTM 33N / WGS84 (Africa)
    (7, -115.0, -50.0, 0.0, 30.0, 14, false), // UTM 14N / WGS84 (Central America)
];

/// Fetch a region definition by id, indexed from 1.
pub fn region(region: u32) -> Option<&'static (u32, f64, f64, f64, f64, u32, bool)> {
    REGIONS.iter().find(|(id, ..)| *id == region)
}

// ---------------------------------------------------------------------------
// PROJ Poder/Engsager (etmerc) UTM coordinate conversion
// ---------------------------------------------------------------------------

/// WGS84 ellipsoid parameters (PROJ `+datum=WGS84`).
const WGS84_A: f64 = 6378137.0;
const WGS84_RF: f64 = 298.257223563;

/// Horner evaluation of `sum(p[i] * x^i, i, 0, N)` (PROJ `pj_polyval`).
fn polyval(x: f64, p: &[f64], n: usize) -> f64 {
    let mut y = p[n];
    let mut i = n;
    while i > 0 {
        i -= 1;
        y = y * x + p[i];
    }
    y
}

/// Expansion of the rectifying radius as a series in `n^2`
/// (PROJ `pj_rectifying_radius`).
fn rectifying_radius(n: f64) -> f64 {
    const COEFF_RAD: [f64; 4] = [1.0, 1.0 / 4.0, 1.0 / 64.0, 1.0 / 256.0];
    polyval(n * n, &COEFF_RAD, 3) / (1.0 + n)
}

// Coefficient block C[phi,chi] (conformal -> geographic), PROJ latitudes.cpp
// `coeffs[12..33)`.
const C_PHI_CHI: [f64; 21] = [
    2.0,
    -2.0 / 3.0,
    -2.0,
    116.0 / 45.0,
    26.0 / 45.0,
    -2854.0 / 675.0,
    7.0 / 3.0,
    -8.0 / 5.0,
    -227.0 / 45.0,
    2704.0 / 315.0,
    2323.0 / 945.0,
    56.0 / 15.0,
    -136.0 / 35.0,
    -1262.0 / 105.0,
    73814.0 / 2835.0,
    4279.0 / 630.0,
    -332.0 / 35.0,
    -399572.0 / 14175.0,
    4174.0 / 315.0,
    -144838.0 / 6237.0,
    601676.0 / 22275.0,
];

// Coefficient block C[chi,phi] (geographic -> conformal), PROJ latitudes.cpp
// `coeffs[87..108)`.
const C_CHI_PHI: [f64; 21] = [
    -2.0,
    2.0 / 3.0,
    4.0 / 3.0,
    -82.0 / 45.0,
    32.0 / 45.0,
    4642.0 / 4725.0,
    5.0 / 3.0,
    -16.0 / 15.0,
    -13.0 / 9.0,
    904.0 / 315.0,
    -1522.0 / 945.0,
    -26.0 / 15.0,
    34.0 / 21.0,
    8.0 / 5.0,
    -12686.0 / 2835.0,
    1237.0 / 630.0,
    -12.0 / 5.0,
    -24832.0 / 14175.0,
    -734.0 / 315.0,
    109598.0 / 31185.0,
    444337.0 / 155925.0,
];

// Coefficient block C[mu,chi] (conformal -> rectifying), PROJ latitudes.cpp
// `coeffs[66..87)`.
const C_MU_CHI: [f64; 21] = [
    1.0 / 2.0,
    -2.0 / 3.0,
    5.0 / 16.0,
    41.0 / 180.0,
    -127.0 / 288.0,
    7891.0 / 37800.0,
    13.0 / 48.0,
    -3.0 / 5.0,
    557.0 / 1440.0,
    281.0 / 630.0,
    -1983433.0 / 1935360.0,
    61.0 / 240.0,
    -103.0 / 140.0,
    15061.0 / 26880.0,
    167603.0 / 181440.0,
    49561.0 / 161280.0,
    -179.0 / 168.0,
    6601661.0 / 7257600.0,
    34729.0 / 80640.0,
    -3418889.0 / 1995840.0,
    212378941.0 / 319334400.0,
];

// Coefficient block C[chi,mu] (rectifying -> conformal), PROJ latitudes.cpp
// `coeffs[108..129)`.
const C_CHI_MU: [f64; 21] = [
    -1.0 / 2.0,
    2.0 / 3.0,
    -37.0 / 96.0,
    1.0 / 360.0,
    81.0 / 512.0,
    -96199.0 / 604800.0,
    -1.0 / 48.0,
    -1.0 / 15.0,
    437.0 / 1440.0,
    -46.0 / 105.0,
    1118711.0 / 3870720.0,
    -17.0 / 480.0,
    37.0 / 840.0,
    209.0 / 4480.0,
    -5569.0 / 90720.0,
    -4397.0 / 161280.0,
    11.0 / 504.0,
    830251.0 / 7257600.0,
    -4583.0 / 161280.0,
    108847.0 / 3991680.0,
    -20648693.0 / 638668800.0,
];

const ETMERC_ORDER: usize = 6;

/// Build the `F[]` coefficient array for converting between auxiliary
/// latitudes (PROJ `pj_auxlat_coeffs`, "else" branch which applies to all
/// conversions involving the conformal latitude).
fn auxlat_coeffs(n: f64, block: &[f64; 21]) -> [f64; 6] {
    let mut f = [0.0f64; ETMERC_ORDER];
    let mut d = n;
    let mut o = 0usize;
    for (l, cell) in f.iter_mut().enumerate() {
        let m = ETMERC_ORDER - l - 1;
        *cell = d * polyval(n, &block[o..o + m + 1], m);
        o += m + 1;
        d *= n;
    }
    f
}

/// Evaluate `sum(F[k] * sin((2k+2)*zeta), k, 0, K-1)` via Clenshaw summation
/// (PROJ `pj_clenshaw`).
fn clenshaw(szeta: f64, czeta: f64, f: &[f64], k: usize) -> f64 {
    let mut u1 = 0.0f64;
    let mut u0 = 0.0f64;
    let x = 2.0 * (czeta - szeta) * (czeta + szeta); // 2 * cos(2*zeta)
    let mut i = k;
    while i > 0 {
        i -= 1;
        let t = x * u0 - u1 + f[i];
        u1 = u0;
        u0 = t;
    }
    2.0 * szeta * czeta * u0
}

/// Convert between auxiliary latitudes, scalar form (PROJ
/// `pj_auxlat_convert`).
fn auxlat_convert(zeta: f64, f: &[f64]) -> f64 {
    zeta + clenshaw(zeta.sin(), zeta.cos(), f, ETMERC_ORDER)
}

/// Convert between auxiliary latitudes, passing sine/cosine of `zeta` (PROJ
/// `pj_auxlat_convert` with explicit `szeta`/`czeta`).
fn auxlat_convert_sincos(zeta: f64, szeta: f64, czeta: f64, f: &[f64]) -> f64 {
    zeta + clenshaw(szeta, czeta, f, ETMERC_ORDER)
}

/// Complex Clenshaw summation (PROJ `clenS`).
#[allow(clippy::too_many_arguments)]
fn clen_s(
    a: &[f64],
    size: usize,
    sin_arg_r: f64,
    cos_arg_r: f64,
    sinh_arg_i: f64,
    cosh_arg_i: f64,
) -> (f64, f64) {
    // arguments
    let mut r = 2.0 * cos_arg_r * cosh_arg_i;
    let i = -2.0 * sin_arg_r * sinh_arg_i;

    // summation loop
    let mut hr1 = 0.0f64;
    let mut hi1 = 0.0f64;
    let mut hi = 0.0f64;
    let mut hr = a[size - 1];
    let mut p = size - 1;
    while p > 0 {
        p -= 1;
        let hr2 = hr1;
        let hi2 = hi1;
        hr1 = hr;
        hi1 = hi;
        hr = -hr2 + r * hr1 - i * hi1 + a[p];
        hi = -hi2 + i * hr1 + r * hi1;
    }

    r = sin_arg_r * cosh_arg_i;
    let i2 = cos_arg_r * sinh_arg_i;
    (r * hr - i2 * hi, r * hi + i2 * hr)
}

/// A UTM zone converter using PROJ's Poder/Engsager algorithm (WGS84).
pub struct UtmConverter {
    quasinorthing: f64, // Qn
    zb: f64,            // Zb
    cgb: [f64; 6],      // conformal -> geographic
    cbg: [f64; 6],      // geographic -> conformal
    utg: [f64; 6],      // rectifying -> conformal
    gtu: [f64; 6],      // conformal -> rectifying
    lam0: f64,          // central meridian (radians)
    x0: f64,
    y0: f64,
}

impl UtmConverter {
    /// Construct a converter for the given UTM zone (1..=60).
    pub fn new(zone: u32, southern: bool) -> Self {
        let f = 1.0 / WGS84_RF;
        let es = f * (2.0 - f);
        let e = es.sqrt();
        let alpha = e.asin();
        // third flattening (PROJ ell_set.cpp: `n = pow(tan(alpha/2), 2)`)
        let n = (alpha / 2.0).tan().powi(2);

        let k0 = 0.9996;
        // `pj_rectifying_radius` is dimensionless (unit ellipsoid); PROJ scales
        // the final result by the semi-major axis to obtain metres.
        let quasinorthing = k0 * WGS84_A * rectifying_radius(n);

        let cgb = auxlat_coeffs(n, &C_PHI_CHI);
        let cbg = auxlat_coeffs(n, &C_CHI_PHI);
        let utg = auxlat_coeffs(n, &C_CHI_MU);
        let gtu = auxlat_coeffs(n, &C_MU_CHI);

        // PROJ utm.cpp: lam0 = (zone + 0.5) * pi / 30 - pi (zone is 0-based)
        let lam0 = (zone as f64 - 0.5) * PI / 30.0 - PI;
        let x0 = 500000.0;
        let y0 = if southern { 10_000_000.0 } else { 0.0 };

        // Gaussian latitude value of the origin latitude (phi0 = 0 for UTM)
        let z = auxlat_convert(0.0, &cbg);
        // origin northing minus true northing at the origin latitude
        let zb = -quasinorthing * auxlat_convert(z, &gtu);

        UtmConverter {
            quasinorthing,
            zb,
            cgb,
            cbg,
            utg,
            gtu,
            lam0,
            x0,
            y0,
        }
    }

    /// Project lon/lat (degrees) to easting/northing (metres).
    pub fn forward(&self, lat: f64, lon: f64) -> (f64, f64) {
        let phi = lat * DEG_TO_RAD;
        let lam = lon * DEG_TO_RAD - self.lam0;

        // ell. LAT, LNG -> Gaussian LAT, LNG
        let mut cn = auxlat_convert(phi, &self.cbg);
        // Gaussian LAT, LNG -> compl. sph. LAT
        let sin_cn = cn.sin();
        let cos_cn = cn.cos();
        let sin_ce = lam.sin();
        let cos_ce = lam.cos();

        let cos_cn_cos_ce = cos_cn * cos_ce;
        cn = sin_cn.atan2(cos_cn_cos_ce);

        let inv_denom_tan_ce = 1.0 / sin_cn.hypot(cos_cn_cos_ce);
        let tan_ce = sin_ce * cos_cn * inv_denom_tan_ce;

        // compl. sph. N, E -> ell. norm. N, E
        let mut ce = tan_ce.asinh();

        let two_inv_denom_tan_ce = 2.0 * inv_denom_tan_ce;
        let two_inv_denom_tan_ce_square = two_inv_denom_tan_ce * inv_denom_tan_ce;
        let tmp_r = cos_cn_cos_ce * two_inv_denom_tan_ce_square;
        let sin_arg_r = sin_cn * tmp_r;
        let cos_arg_r = cos_cn_cos_ce * tmp_r - 1.0;

        let sinh_arg_i = tan_ce * two_inv_denom_tan_ce;
        let cosh_arg_i = two_inv_denom_tan_ce_square - 1.0;

        let (dcn, dce) = clen_s(&self.gtu, ETMERC_ORDER, sin_arg_r, cos_arg_r, sinh_arg_i, cosh_arg_i);
        cn += dcn;
        ce += dce;
        if ce.abs() <= 2.623395162778 {
            (self.quasinorthing * ce + self.x0, self.quasinorthing * cn + self.zb + self.y0)
        } else {
            (f64::NAN, f64::NAN) // outside projection domain
        }
    }

    /// Inverse project easting/northing (metres) to lon/lat (degrees).
    pub fn inverse(&self, easting: f64, northing: f64) -> (f64, f64) {
        // normalize N, E
        let mut cn = (northing - self.y0 - self.zb) / self.quasinorthing;
        let mut ce = (easting - self.x0) / self.quasinorthing;

        if ce.abs() <= 2.623395162778 {
            // norm. N, E -> compl. sph. LAT, LNG
            let sin_arg_r = (2.0 * cn).sin();
            let cos_arg_r = (2.0 * cn).cos();

            let exp_2_ce = (2.0 * ce).exp();
            let half_inv_exp_2_ce = 0.5 / exp_2_ce;
            let sinh_arg_i = 0.5 * exp_2_ce - half_inv_exp_2_ce;
            let cosh_arg_i = 0.5 * exp_2_ce + half_inv_exp_2_ce;

            let (_dcn_ignored, dce) = clen_s(&self.utg, ETMERC_ORDER, sin_arg_r, cos_arg_r, sinh_arg_i, cosh_arg_i);
            cn += _dcn_ignored;
            ce += dce;

            // compl. sph. LAT -> Gaussian LAT, LNG
            let sin_cn = cn.sin();
            let cos_cn = cn.cos();

            let sinh_ce = ce.sinh();
            ce = sinh_ce.atan2(cos_cn);
            let modulus_ce = sinh_ce.hypot(cos_cn);
            let rr = sin_cn.hypot(modulus_ce);
            cn = sin_cn.atan2(modulus_ce);

            // Gaussian LAT, LNG -> ell. LAT, LNG
            let phi = auxlat_convert_sincos(cn, sin_cn / rr, modulus_ce / rr, &self.cgb);
            (phi / DEG_TO_RAD, self.lam0 / DEG_TO_RAD + ce / DEG_TO_RAD)
        } else {
            (f64::NAN, f64::NAN)
        }
    }
}

// ---------------------------------------------------------------------------
// Envelope / Grid / GridFactory
// ---------------------------------------------------------------------------

/// Rectangular lon/lat envelope (ported from `blitzortung.geom.Envelope`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Envelope {
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
}

impl Envelope {
    pub fn new(x_min: f64, x_max: f64, y_min: f64, y_max: f64) -> Self {
        Envelope {
            x_min,
            x_max,
            y_min,
            y_max,
        }
    }

    pub fn y_delta(&self) -> f64 {
        (self.y_max - self.y_min).abs()
    }

    pub fn x_delta(&self) -> f64 {
        (self.x_max - self.x_min).abs()
    }

    /// The envelope as a WKB LinearRing, matching
    /// `shapely.geometry.LinearRing([(xmin,ymin),(xmin,ymax),(xmax,ymax),
    /// (xmax,ymin)])`.
    pub fn as_wkb_linear_ring(&self) -> Vec<u8> {
        crate::wkb::linear_ring(&[
            [self.x_min, self.y_min],
            [self.x_min, self.y_max],
            [self.x_max, self.y_max],
            [self.x_max, self.y_min],
        ])
    }
}

/// Grid characteristics (ported from `blitzortung.geom.Grid`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub x_min: f64,
    pub x_max: f64,
    pub y_min: f64,
    pub y_max: f64,
    pub x_div: f64,
    pub y_div: f64,
}

impl Grid {
    pub fn new(x_min: f64, x_max: f64, y_min: f64, y_max: f64, x_div: f64, y_div: f64) -> Self {
        Grid {
            x_min,
            x_max,
            y_min,
            y_max,
            x_div,
            y_div,
        }
    }

    fn get_x_bin(&self, x_pos: f64) -> i64 {
        ((x_pos - self.x_min) / self.x_div).ceil() as i64 - 1
    }

    fn get_y_bin(&self, y_pos: f64) -> i64 {
        ((y_pos - self.y_min) / self.y_div).ceil() as i64 - 1
    }

    pub fn x_bin_count(&self) -> i64 {
        self.get_x_bin(self.x_max) + 1
    }

    pub fn y_bin_count(&self) -> i64 {
        self.get_y_bin(self.y_max) + 1
    }

    pub fn get_x_center(&self, cell_index: i64) -> f64 {
        self.x_min + (cell_index as f64 + 0.5) * self.x_div
    }

    pub fn get_y_center(&self, row_index: i64) -> f64 {
        self.y_min + (row_index as f64 + 0.5) * self.y_div
    }

    pub fn envelope(&self) -> Envelope {
        Envelope::new(self.x_min, self.x_max, self.y_min, self.y_max)
    }
}

/// Builds per-region grids for a given base length
/// (ported from `blitzortung.geom.GridFactory`).
#[derive(Debug, Clone, Copy)]
pub struct GridFactory {
    pub min_lon: f64,
    pub max_lon: f64,
    pub min_lat: f64,
    pub max_lat: f64,
    pub zone: u32,
    pub southern: bool,
    /// Optional explicit reference longitude/latitude (defaults to the
    /// midpoint of the bounds).
    pub ref_lon: Option<f64>,
    pub ref_lat: Option<f64>,
}

impl GridFactory {
    /// Create a factory, clamping the bounds to the WGS84 range exactly like
    /// `GridFactory.__init__` (`min_lon = max(-180, min_lon)` etc.).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        min_lon: f64,
        max_lon: f64,
        min_lat: f64,
        max_lat: f64,
        zone: u32,
        southern: bool,
        ref_lon: Option<f64>,
        ref_lat: Option<f64>,
    ) -> Self {
        GridFactory {
            min_lon: min_lon.max(-180.0),
            max_lon: max_lon.min(180.0),
            min_lat: min_lat.max(-90.0),
            max_lat: max_lat.min(90.0),
            zone,
            southern,
            ref_lon,
            ref_lat,
        }
    }

    pub fn for_region(region_id: u32) -> Option<Self> {
        region(region_id).map(|(_, min_lon, max_lon, min_lat, max_lat, zone, southern)| {
            GridFactory::new(
                *min_lon,
                *max_lon,
                *min_lat,
                *max_lat,
                *zone,
                *southern,
                None,
                None,
            )
        })
    }

    /// The world-wide grid factory
    /// (`blitzortung.gis.constants.global_grid`): UTM 33N with explicit
    /// reference point (11, 48).
    pub fn global() -> Self {
        GridFactory::new(-180.0, 180.0, -90.0, 90.0, 33, false, Some(11.0), Some(48.0))
    }

    /// `blitzortung.geom.GridFactory.fix_max`
    pub fn fix_max(minimum: f64, maximum: f64, delta: f64) -> f64 {
        minimum + ((maximum - minimum) / delta).floor() * delta
    }

    /// Build the grid for `base_length` metres
    /// (`blitzortung.geom.GridFactory.get_for`).
    pub fn get_for(&self, base_length: f64) -> Grid {
        let ref_lon = self.ref_lon.unwrap_or_else(|| (self.min_lon + self.max_lon) / 2.0);
        let ref_lat = self.ref_lat.unwrap_or_else(|| (self.min_lat + self.max_lat) / 2.0);

        let utm = UtmConverter::new(self.zone, self.southern);
        let (utm_x, utm_y) = utm.forward(ref_lat, ref_lon);
        let (lat_d, lon_d) = utm.inverse(utm_x + base_length, utm_y + base_length);

        let delta_lon = lon_d - ref_lon;
        let delta_lat = lat_d - ref_lat;

        let max_lon = Self::fix_max(self.min_lon, self.max_lon, delta_lon);
        let max_lat = Self::fix_max(self.min_lat, self.max_lat, delta_lat);

        Grid::new(
            self.min_lon,
            max_lon,
            self.min_lat,
            max_lat,
            delta_lon,
            delta_lat,
        )
    }
}

/// Local grid around an (x, y) tile (ported from
/// `blitzortung/gis/local_grid.py`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalGrid {
    pub data_area: i64,
    pub x: i64,
    pub y: i64,
}

impl LocalGrid {
    pub const DATA_AREA_SIZE_FACTOR: i64 = 3;
    pub const LOCAL_GRID_UTM_LONGITUDE: f64 = 3.0;

    /// `LocalGrid.size`: the grid spans `data_area * 3` degrees.
    pub fn size(&self) -> i64 {
        self.data_area * Self::DATA_AREA_SIZE_FACTOR
    }

    /// `LocalGrid.reference_longitude`
    pub fn reference_longitude(&self) -> f64 {
        ((self.x - 1) * self.data_area) as f64
    }

    /// `LocalGrid.reference_latitude`
    pub fn reference_latitude(&self) -> f64 {
        ((self.y - 1) * self.data_area) as f64
    }

    /// `LocalGrid.center_latitude`
    pub fn center_latitude(&self) -> f64 {
        self.reference_latitude() + self.size() as f64 / 2.0
    }

    /// `LocalGrid.longitude_extension`
    pub fn longitude_extension(&self) -> f64 {
        self.center_latitude().abs() / 15.0
    }

    /// The grid factory for this local grid
    /// (`LocalGrid.get_grid_factory`, `_create_grid_factory`).  The Python
    /// layer LRU-caches factories by `(data_area, x, y)` (maxsize 1024); here
    /// the factory is deterministic so it is rebuilt per call, which is
    /// behaviourally equivalent (the UTM converter is cheap to construct).
    pub fn grid_factory(&self) -> GridFactory {
        let reference_latitude = self.reference_latitude();
        let extension = self.longitude_extension();
        let size = self.size() as f64;
        GridFactory::new(
            self.reference_longitude() - extension,
            self.reference_longitude() + size + extension,
            reference_latitude,
            reference_latitude + size,
            31,
            reference_latitude < 0.0,
            Some(Self::LOCAL_GRID_UTM_LONGITUDE),
            Some(reference_latitude + size / 2.0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference values generated with pyproj 3.6.1 (PROJ 9.x) using the
    // exact same procedure as `blitzortung.geom.GridFactory.get_for`
    // against the origin/main region table (see gen_grid_ref.py in the task
    // notes).
    const REF: &[(u32, f64, f64, f64, f64, f64)] = &[
        (1, 0.14017221762500753, 0.08865376938211966, 56.8605750930044, 71.94746107673467, 10000.0),
        (2, 0.10042732079560324, 0.08896248521472572, 179.99784259453546, -0.00308330932414691, 10000.0),
        (3, 0.11382317953352583, 0.08628400349865473, -50.07968816851459, 59.95843802572109, 10000.0),
        (4, 0.09953271581591139, 0.08993934856256303, 149.99486342779014, 59.97281318167404, 10000.0),
        (5, 0.09376108435947117, 0.08956457406750751, -30.054231067834507, 19.949932346723358, 10000.0),
        (6, 0.08986754959421539, 0.09047302416644964, 49.91695358429958, 39.97815336314149, 10000.0),
        (7, 0.09603562976465696, 0.07996095890690214, -50.079914279091895, 29.9853595900883, 10000.0),
    ];

    // Reference values for the factory used by `get_global_strikes_grid`
    // (GridFactory(-180, 180, -90, 90, epsg:32633, ref_lon=11, ref_lat=48)).
    const REF_GLOBAL: &[(f64, f64, f64, f64, f64)] = &[
        (25000.0, 0.3183994501281493, 0.2356365657886883, 179.79137864480873, 89.79069969676917),
        (50000.0, 0.6397258555172272, 0.4704490373267589, 179.52593080068164, 89.7115322588219),
    ];

    // Reference values for the local grid factory with data_area=5, x=5, y=5
    // (epsg:32631, ref_lon=3, ref_lat=27.5).
    const REF_LOCAL: (f64, f64, f64, f64, f64) = (
        0.10132537303619804,
        0.09024194706370281,
        36.81053530532711,
        34.980163212574666,
        10000.0,
    );

    #[test]
    fn forward_matches_pyproj_reference_zone33() {
        let utm = UtmConverter::new(33, false);
        // (lat, lon) -> (easting, northing); reference: pyproj
        // transform(12.34, 45.67) for epsg:32633
        let (e, n) = utm.forward(45.67, 12.34);
        let (elat, elon) = utm.inverse(e, n);
        assert!((elat - 45.67).abs() < 1e-9, "lat {elat}");
        assert!((elon - 12.34).abs() < 1e-9, "lon {elon}");
    }

    #[test]
    fn grid_matches_pyproj_reference() {
        for &(region, x_div, y_div, x_max, y_max, base_length) in REF {
            let factory = GridFactory::for_region(region).expect("region exists");
            let grid = factory.get_for(base_length);
            assert!(
                (grid.x_div - x_div).abs() < 1e-12,
                "region {region} x_div: expected {x_div}, got {}",
                grid.x_div
            );
            assert!(
                (grid.y_div - y_div).abs() < 1e-12,
                "region {region} y_div: expected {y_div}, got {}",
                grid.y_div
            );
            assert!(
                (grid.x_max - x_max).abs() < 1e-9,
                "region {region} x_max: expected {x_max}, got {}",
                grid.x_max
            );
            assert!(
                (grid.y_max - y_max).abs() < 1e-9,
                "region {region} y_max: expected {y_max}, got {}",
                grid.y_max
            );
        }
    }

    #[test]
    fn fix_max_rounds_down() {
        assert_eq!(GridFactory::fix_max(0.0, 10.0, 3.0), 9.0);
        assert_eq!(GridFactory::fix_max(-20.0, 57.0, 0.14), -20.0 + 550.0 * 0.14);
    }

    #[test]
    fn global_grid_matches_pyproj_reference() {
        let factory = GridFactory::global();
        assert_eq!((factory.ref_lon, factory.ref_lat), (Some(11.0), Some(48.0)));
        for &(base_length, x_div, y_div, x_max, y_max) in REF_GLOBAL {
            let grid = factory.get_for(base_length);
            assert!(
                (grid.x_div - x_div).abs() < 1e-12,
                "global {base_length} x_div: expected {x_div}, got {}",
                grid.x_div
            );
            assert!(
                (grid.y_div - y_div).abs() < 1e-12,
                "global {base_length} y_div: expected {y_div}, got {}",
                grid.y_div
            );
            assert!(
                (grid.x_max - x_max).abs() < 1e-9,
                "global {base_length} x_max: expected {x_max}, got {}",
                grid.x_max
            );
            assert!(
                (grid.y_max - y_max).abs() < 1e-9,
                "global {base_length} y_max: expected {y_max}, got {}",
                grid.y_max
            );
        }
    }

    #[test]
    fn local_grid_factory_matches_pyproj_reference() {
        let local = LocalGrid {
            data_area: 5,
            x: 5,
            y: 5,
        };
        // derived properties pin the local_grid.py formulas
        assert_eq!(local.size(), 15);
        assert_eq!(local.reference_longitude(), 20.0);
        assert_eq!(local.reference_latitude(), 20.0);
        assert_eq!(local.center_latitude(), 27.5);
        assert_eq!(local.longitude_extension(), 27.5 / 15.0);

        let factory = local.grid_factory();
        assert_eq!(factory.zone, 31);
        assert!(!factory.southern); // ref_lat (27.5) >= 0 -> UTM 31 N
        let (x_div, y_div, x_max, y_max, base_length) = REF_LOCAL;
        let grid = factory.get_for(base_length);
        assert!(
            (grid.x_div - x_div).abs() < 1e-12,
            "local x_div: expected {x_div}, got {}",
            grid.x_div
        );
        assert!(
            (grid.y_div - y_div).abs() < 1e-12,
            "local y_div: expected {y_div}, got {}",
            grid.y_div
        );
        assert!(
            (grid.x_max - x_max).abs() < 1e-9,
            "local x_max: expected {x_max}, got {}",
            grid.x_max
        );
        assert!(
            (grid.y_max - y_max).abs() < 1e-9,
            "local y_max: expected {y_max}, got {}",
            grid.y_max
        );
    }

    #[test]
    fn factory_bounds_are_clamped_to_wgs84() {
        let factory = GridFactory::new(-200.0, 200.0, -100.0, 100.0, 33, false, None, None);
        assert_eq!(factory.min_lon, -180.0);
        assert_eq!(factory.max_lon, 180.0);
        assert_eq!(factory.min_lat, -90.0);
        assert_eq!(factory.max_lat, 90.0);
    }
}