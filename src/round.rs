//! Python-compatible rounding helpers.
//!
//! The Python service layer uses `round(x, ndigits)` and `strftime`
//! formatting, which differ subtly from the default Rust float formatting.

/// Python `round(x, ndigits)`.
///
/// CPython rounds the *exact* binary value of `x` to `ndigits` decimals with
/// round-half-to-even (implemented via David Gay's `_Py_dg_dtoa`), rather than
/// multiplying by `10**ndigits` first.  A naive `(x * 10**n).round()/10**n`
/// disagrees with CPython for values such as `round(0.025, 2)` (the binary
/// value is slightly *above* 0.025, so CPython returns 0.03).  Rust's
/// precision float formatting rounds the exact value with ties-to-even, which
/// matches: we format to `ndigits` decimals and parse the shortest round-trip
/// representation back.
pub fn py_round(x: f64, ndigits: i32) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if ndigits >= 0 {
        format!("{:.*}", ndigits as usize, x).parse::<f64>().unwrap()
    } else {
        let scale = 10f64.powi(-ndigits);
        py_round(x / scale, 0) * scale
    }
}

/// Format a float with a fixed number of decimals, matching Python's
/// `"%.{n}f" % value` output (round-half-even on the exact value).
pub fn py_format_fixed(x: f64, decimals: usize) -> String {
    format!("{:.*}", decimals, x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_half_even_behavior() {
        // Python: round(0.5) == 0, round(1.5) == 2, round(2.5) == 2
        assert_eq!(py_round(0.5, 0), 0.0);
        assert_eq!(py_round(1.5, 0), 2.0);
        assert_eq!(py_round(2.5, 0), 2.0);
        assert_eq!(py_round(0.125, 2), 0.12);
        // 0.025 is stored as slightly above 0.025, so it rounds up (matches
        // CPython); a multiply-based implementation would give 0.02.
        assert_eq!(py_round(0.025, 2), 0.03);
        // Python: round(2.675, 2) == 2.67 due to binary representation
        assert_eq!(py_round(2.675, 2), 2.67);
    }

    #[test]
    fn ordinary_rounding() {
        assert_eq!(py_round(0.14154429005533942, 6), 0.141544);
        assert_eq!(py_round(0.08558925740048551, 6), 0.085589);
        assert_eq!(py_round(56.85854950004931, 4), 56.8585);
        assert_eq!(py_round(69.96580721504372, 4), 69.9658);
        assert_eq!(py_round(179.99784259453546, 4), 179.9978);
        assert_eq!(py_round(-0.00308330932414691, 4), -0.0031);
        assert_eq!(py_round(49.91695358429958, 4), 49.917);
    }

    #[test]
    fn negative_ndigits() {
        // Python: round(1234, -2) == 1200, round(1250, -2) == 1200 (ties to
        // even at the hundreds digit)
        assert_eq!(py_round(1234.0, -2), 1200.0);
        assert_eq!(py_round(1250.0, -2), 1200.0);
        assert_eq!(py_round(1350.0, -2), 1400.0);
    }

    #[test]
    fn format_matches_python_percent_formatting() {
        assert_eq!(py_format_fixed(56.85854950004931, 4), "56.8585");
        assert_eq!(py_format_fixed(2.675, 2), "2.67");
        assert_eq!(py_format_fixed(-0.003, 4), "-0.0030");
    }
}