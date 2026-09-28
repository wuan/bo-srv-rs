//! Minimal WKB (Well-Known Binary) encoder.
//!

/// Encode a 2D WKB Polygon (with optional interior rings) in the ISO/OGC
/// little-endian layout, matching `shapely.wkb.dumps(Polygon(...))`.
///
/// Layout: `01 03000000 <num_rings:u32le> (<num_points:u32le>
/// (<x:f64le> <y:f64le>)*)*`.  Each ring is closed automatically.
pub fn polygon(rings: &[Vec<[f64; 2]>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0x01); // little endian
    out.extend_from_slice(&3u32.to_le_bytes()); // Polygon
    out.extend_from_slice(&(rings.len() as u32).to_le_bytes());
    for ring in rings {
        let mut closed: Vec<[f64; 2]> = ring.to_vec();
        if let Some(first) = ring.first() {
            if ring.last() != Some(first) {
                closed.push(*first);
            }
        }
        out.extend_from_slice(&(closed.len() as u32).to_le_bytes());
        for p in &closed {
            out.extend_from_slice(&p[0].to_le_bytes());
            out.extend_from_slice(&p[1].to_le_bytes());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_polygon_with_expected_layout() {
        let wkb = polygon(&[vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]]);
        assert_eq!(wkb[0], 0x01);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 1); // rings
        assert_eq!(u32::from_le_bytes(wkb[9..13].try_into().unwrap()), 5); // closed
    }
}
