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

/// Encode a 2D WKB LineString (or a closed LinearRing) in the ISO/OGC
/// little-endian layout, matching `shapely.wkb.dumps(LinearRing(...))`.
///
/// Layout: `01 02000000 <num_points:u32le> (<x:f64le> <y:f64le>)*`.  Shapely
/// encodes a `LinearRing` with the geometry type code of a `LineString` (2),
/// so the cluster `shape` round-trips through `ST_GeomFromWKB`/`ST_AsBinary`
/// as a LineString.
pub fn linestring(points: &[(f64, f64)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(9 + points.len() * 16);
    out.push(0x01); // little endian
    out.extend_from_slice(&2u32.to_le_bytes()); // LineString
    out.extend_from_slice(&(points.len() as u32).to_le_bytes());
    for (x, y) in points {
        out.extend_from_slice(&x.to_le_bytes());
        out.extend_from_slice(&y.to_le_bytes());
    }
    out
}

/// Decode an ISO/OGC WKB LineString (type code 2) into its ordered
/// coordinates.  Returns `None` for a different geometry type, the extended
/// (EWKB) layout with an SRID flag, or a truncated buffer.
///
/// `ST_AsBinary` returns ISO WKB in the server byte order, so both
/// endiannesses are accepted.
pub fn decode_linestring(wkb: &[u8]) -> Option<Vec<(f64, f64)>> {
    if wkb.len() < 9 {
        return None;
    }
    let little_endian = match wkb[0] {
        0x01 => true,
        0x00 => false,
        _ => return None,
    };
    let read_u32 = |offset: usize| -> Option<u32> {
        let bytes: [u8; 4] = wkb.get(offset..offset + 4)?.try_into().ok()?;
        Some(if little_endian {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        })
    };
    let read_f64 = |offset: usize| -> Option<f64> {
        let bytes: [u8; 8] = wkb.get(offset..offset + 8)?.try_into().ok()?;
        Some(if little_endian {
            f64::from_le_bytes(bytes)
        } else {
            f64::from_be_bytes(bytes)
        })
    };

    let geometry_type = read_u32(1)?;
    // Reject EWKB (high bit set) and anything that is not a LineString.
    if geometry_type & 0xE000_0000 != 0 || geometry_type != 2 {
        return None;
    }
    let point_count = read_u32(5)? as usize;
    let mut points = Vec::with_capacity(point_count);
    for i in 0..point_count {
        let base = 9 + i * 16;
        points.push((read_f64(base)?, read_f64(base + 8)?));
    }
    Some(points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_linestring_with_expected_layout() {
        let wkb = linestring(&[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]);
        assert_eq!(wkb[0], 0x01);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 2); // LineString
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 3); // points
        assert_eq!(wkb.len(), 9 + 3 * 16);
    }

    #[test]
    fn encodes_polygon_with_expected_layout() {
        let wkb = polygon(&[vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]]);
        assert_eq!(wkb[0], 0x01);
        assert_eq!(u32::from_le_bytes(wkb[1..5].try_into().unwrap()), 3);
        assert_eq!(u32::from_le_bytes(wkb[5..9].try_into().unwrap()), 1); // rings
        assert_eq!(u32::from_le_bytes(wkb[9..13].try_into().unwrap()), 5); // closed
    }

    #[test]
    fn linestring_round_trips() {
        let points = vec![(11.0, 51.0), (11.1, 51.0), (11.1, 51.1), (11.0, 51.0)];
        let encoded = linestring(&points);
        assert_eq!(decode_linestring(&encoded), Some(points));
    }

    #[test]
    fn decode_linestring_rejects_other_types_and_truncation() {
        let polygon = polygon(&[vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]]);
        assert_eq!(decode_linestring(&polygon), None);
        assert_eq!(decode_linestring(&[]), None);
        assert_eq!(decode_linestring(&[0x01, 0x02, 0x00]), None);
        // Truncated point payload.
        let encoded = linestring(&[(0.0, 0.0)]);
        assert_eq!(decode_linestring(&encoded[..encoded.len() - 1]), None);
    }
}
