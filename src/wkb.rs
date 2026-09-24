//! Minimal WKB (Well-Known Binary) encoder.
//!
//! The Python layer uses `shapely.wkb.dumps(LinearRing([...]))` to build the
//! envelope parameter for PostGIS `ST_GeomFromWKB`.  Shapely emits 2D
//! geometries by default (keep the 2D/3D discrepancy in mind: shapely points
//! have 2 coordinates unless explicitly Z; the envelope LinearRing is built
//! from 2-tuples, so it is a 2D ring with byte order 0x01 and type 0x02,
//! i.e. a closed LineString).

/// Encode a closed 2D WKB LinearRing (ISO/OGC variant used by Shapely 2.x).
///
/// Layout: `01 02000000 <num_points:u32le> (<x:f64le> <y:f64le>)*`.
/// Shapely 1.x/2.x `wkb.dumps(LinearRing(...))` emits a ring with the ring
/// type code `2` (LineString) since `LinearRing` dumps as a LineString ring.
pub fn linear_ring(points: &[[f64; 2]]) -> Vec<u8> {
    // Build a closed ring: append the first point again at the end like
    // Shapely's LinearRing does.
    let mut closed: Vec<[f64; 2]> = points.to_vec();
    if let Some(first) = points.first() {
        closed.push(*first);
    }
    let mut out = Vec::with_capacity(1 + 4 + 4 + closed.len() * 16);
    out.push(0x01); // little endian
    out.extend_from_slice(&2u32.to_le_bytes()); // LineString (ring)
    out.extend_from_slice(&(closed.len() as u32).to_le_bytes());
    for p in &closed {
        out.extend_from_slice(&p[0].to_le_bytes());
        out.extend_from_slice(&p[1].to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_ring_with_expected_layout() {
        let wkb = linear_ring(&[[1.0, 2.0], [3.0, 4.0]]);
        assert_eq!(wkb.len(), 1 + 4 + 4 + 3 * 16);
        assert_eq!(wkb[0], 0x01);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 2);
        let n = u32::from_le_bytes(wkb[5..9].try_into().unwrap());
        assert_eq!(n, 3); // first point repeated to close the ring
        assert_eq!(
            f64::from_le_bytes(wkb[9..17].try_into().unwrap()),
            1.0
        );
        assert_eq!(
            f64::from_le_bytes(wkb[17..25].try_into().unwrap()),
            2.0
        );
    }
}