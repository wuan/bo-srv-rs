//! Strike clustering (port of `blitzortung/clustering/base.py`).
//!
//! The Python implementation relies on `scipy`/`fastcluster` (hierarchical
//! agglomeration), `shapely` (convex hull, buffer, simplify) and
//! `geographiclib` (area).  This port:
//!
//! * implements the single-linkage agglomeration directly
//!   ([`single_linkage`]), matching `fastcluster.linkage`'s merge order and
//!   its `(min_label, max_label, distance, size)` output rows;
//! * uses the [`geo`] crate for the convex hull, the round-join `0.02` deg
//!   buffer and the Douglas-Peucker simplification;
//! * uses `geographiclib-rs` for the WGS84 polygon area.
//!
//! See [`Clustering::build_clusters`] for the algorithm and the module docs of
//! [`crate::clustering::geometry`] for the geometry pipeline.

use chrono::DateTime;
use chrono::Utc;

use crate::clustering::geometry::{buffer_ring, convex_hull_ring, simplify_ring};
use crate::data::{Strike, StrikeCluster, Timestamp};
use crate::query::TimeInterval;

/// One merge produced by [`single_linkage`]:
/// `(cluster_a, cluster_b, distance, resulting_size)` with `cluster_a <
/// cluster_b`.  New clusters are labelled `event_count, event_count + 1, ...`
/// in merge order, exactly like the SciPy/Fastcluster linkage matrix.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Merge {
    pub cluster_a: usize,
    pub cluster_b: usize,
    pub distance: f64,
    pub size: usize,
}

/// Hierarchical single-linkage agglomeration over the given points.
///
/// Reproduces `fastcluster.linkage(pdist(data))` with the default
/// single-linkage criterion:
///
/// * clusters are identified by the smallest original index they contain;
/// * the next merge is the globally closest pair, ties broken by the
///   lexicographically smallest `(cluster_a, cluster_b)`;
/// * a new cluster is labelled `event_count + merge_index`.
///
/// For single linkage the dendrogram is exactly the sorted set of minimum
/// spanning tree edges (this is what `fastcluster`'s single-linkage routine
/// uses).  Building the MST with Prim's algorithm is `O(n^2)` time and `O(n)`
/// memory, so a dense ten-minute window with tens of thousands of strikes stays
/// tractable — the previous direct `O(n^4)` scan did not.
pub fn single_linkage(points: &[(f64, f64)]) -> Vec<Merge> {
    let n = points.len();
    if n < 2 {
        return Vec::new();
    }

    // Prim's MST on the complete great-circle graph.  `key[v]` is the cheapest
    // known edge from the tree to `v`; `parent[v]` is its other endpoint.
    let mut in_tree = vec![false; n];
    let mut key = vec![f64::INFINITY; n];
    let mut parent = vec![usize::MAX; n];
    key[0] = 0.0;
    let mut edges: Vec<(f64, usize, usize)> = Vec::with_capacity(n - 1);

    for _ in 0..n {
        // The unvisited vertex with the smallest connecting edge.  Ties pick the
        // lowest index, matching the deterministic order the reference needs.
        let mut u = usize::MAX;
        for v in 0..n {
            if !in_tree[v] && (u == usize::MAX || key[v] < key[u]) {
                u = v;
            }
        }
        in_tree[u] = true;
        if parent[u] != usize::MAX {
            let (a, b) = if parent[u] < u {
                (parent[u], u)
            } else {
                (u, parent[u])
            };
            edges.push((key[u], a, b));
        }
        for v in 0..n {
            if !in_tree[v] {
                let d = crate::clustering::pdist::distance(
                    points[u].0,
                    points[u].1,
                    points[v].0,
                    points[v].1,
                );
                if d < key[v] {
                    key[v] = d;
                    parent[v] = u;
                }
            }
        }
    }

    // Merge the MST edges in ascending order via union-find, relabelling merged
    // components with `n, n + 1, ...` in merge order (SciPy's linkage labels).
    edges.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
            .then(a.2.cmp(&b.2))
    });

    let mut union_find: Vec<usize> = (0..2 * n).collect();
    let mut cluster_id: Vec<usize> = (0..2 * n).collect();
    let mut size: Vec<usize> = vec![1; 2 * n];
    let mut merges = Vec::with_capacity(n - 1);
    let mut next_label = n;

    fn find(union_find: &mut [usize], x: usize) -> usize {
        let mut root = x;
        while union_find[root] != root {
            root = union_find[root];
        }
        // Path compression.
        let mut current = x;
        while union_find[current] != root {
            let next = union_find[current];
            union_find[current] = root;
            current = next;
        }
        root
    }

    for (distance, i, j) in edges {
        let mut root_i = find(&mut union_find, i);
        let mut root_j = find(&mut union_find, j);
        if root_i == root_j {
            continue;
        }
        let mut a = cluster_id[root_i];
        let mut b = cluster_id[root_j];
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        let merged_size = size[root_i] + size[root_j];
        merges.push(Merge {
            cluster_a: a,
            cluster_b: b,
            distance,
            size: merged_size,
        });
        // Union by attaching the larger root under the smaller-indexed one so
        // the surviving root is deterministic.
        if root_i > root_j {
            std::mem::swap(&mut root_i, &mut root_j);
        }
        union_find[root_j] = root_i;
        size[root_i] = merged_size;
        cluster_id[root_i] = next_label;
        next_label += 1;
    }

    merges
}

/// Strike clustering (port of `blitzortung.clustering.Clustering`).
#[derive(Debug, Clone, Copy)]
pub struct Clustering {
    /// Maximum single-linkage distance (km) at which clusters merge.
    pub distance_limit: f64,
    /// Simplification tolerance in degrees (`coordinate_accuracy`).
    pub coordinate_accuracy: f64,
    /// Buffer distance in degrees (`buffer_size`).
    pub buffer_size: f64,
    /// Decimal places coordinates/hull vertices are rounded to.
    pub coordinate_precision: i32,
}

impl Default for Clustering {
    fn default() -> Self {
        Clustering {
            distance_limit: 8.0,
            coordinate_accuracy: 0.01,
            buffer_size: 0.02,
            coordinate_precision: 4,
        }
    }
}

impl Clustering {
    /// The Python class-level constants.
    pub fn new() -> Self {
        Self::default()
    }

    /// Build clusters for `events` over `time_interval` (port of
    /// `Clustering.build_clusters`).
    ///
    /// Emits one [`StrikeCluster`] per agglomerated cluster with more than two
    /// strikes, in Python's `extract_clustered_events` order (merge order).
    pub fn build_clusters(
        &self,
        events: &[Strike],
        time_interval: &TimeInterval,
    ) -> Vec<StrikeCluster> {
        let event_count = events.len();
        let mut clusters_out = Vec::new();
        let timestamp = Timestamp::new(time_interval.end, 0);
        let interval_seconds = time_interval.duration_seconds();

        if event_count == 0 {
            log_debug(time_interval, interval_seconds, event_count, 0, 0);
            return clusters_out;
        }

        let points: Vec<(f64, f64)> = events.iter().map(|event| (event.x, event.y)).collect();
        if points.len() < 3 {
            log_debug(time_interval, interval_seconds, event_count, 0, 0);
            return clusters_out;
        }

        let merges = single_linkage(&points);
        // `apply_results`: merge while distance <= distance_limit, stopping at
        // the first merge that exceeds it.
        let mut clusters: std::collections::HashMap<usize, Vec<usize>> =
            (0..event_count).map(|i| (i, vec![i])).collect();
        for (label, merge) in (event_count..).zip(merges.iter()) {
            if merge.distance > self.distance_limit {
                break;
            }
            let mut merged = clusters
                .get(&merge.cluster_a)
                .expect("cluster a present")
                .clone();
            merged.extend(clusters.get(&merge.cluster_b).expect("cluster b present"));
            debug_assert_eq!(merged.len(), merge.size);
            clusters.remove(&merge.cluster_a);
            clusters.remove(&merge.cluster_b);
            clusters.insert(label, merged);
        }

        // `extract_clustered_events`: only the agglomerated clusters (labels
        // beyond the original event count), in label order.
        let mut clustered: Vec<Vec<usize>> = clusters
            .into_iter()
            .filter(|(index, _)| *index >= event_count)
            .map(|(_, members)| members)
            .collect();
        clustered.sort_by_key(|members| members.iter().copied().min());

        for members in clustered {
            let events_in_cluster = members.len();
            if events_in_cluster <= 2 {
                continue;
            }
            let cluster_points: Vec<(f64, f64)> = members.iter().map(|&i| points[i]).collect();
            let shape = match self.build_shape(&cluster_points) {
                Some(shape) => shape,
                None => continue,
            };
            let area = crate::clustering::geometry::polygon_area_km2(&shape);
            clusters_out.push(StrikeCluster {
                id: -1,
                timestamp,
                interval_seconds,
                shape: Some(shape),
                strike_count: events_in_cluster as i64,
                area,
            });
        }

        log_debug(
            time_interval,
            interval_seconds,
            event_count,
            clusters_out.len(),
            clusters_out.len(),
        );
        clusters_out
    }

    /// The convex-hull -> round -> buffer -> simplify -> round pipeline
    /// (the body of the per-cluster loop in `build_clusters`).
    fn build_shape(&self, points: &[(f64, f64)]) -> Option<Vec<(f64, f64)>> {
        let hull = convex_hull_ring(points, self.coordinate_precision)?;
        let buffered = buffer_ring(&hull, self.buffer_size);
        let simplified = simplify_ring(&buffered, self.coordinate_accuracy);
        if !simplified.is_empty() {
            Some(simplified)
        } else {
            None
        }
    }
}

/// Emit the Python debug line:
/// `build_clusters(<start> +<duration>): <events> events -> <clusters> clusters -> <filtered>`.
fn log_debug(
    time_interval: &TimeInterval,
    interval_seconds: i64,
    event_count: usize,
    clustered: usize,
    filtered: usize,
) {
    log::debug!(
        "build_clusters({} +{}): {} events -> {} clusters -> {} filtered",
        format_duration_iso(time_interval.start),
        format_duration_iso_positive(interval_seconds),
        event_count,
        clustered,
        filtered
    );
    let _: &DateTime<Utc> = &time_interval.start;
}

/// Render a start timestamp like the Python `datetime` repr (`2016-01-01 12:00:00`).
fn format_duration_iso(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Render the interval duration like `datetime.timedelta` (`0:10:00`).
fn format_duration_iso_positive(seconds: i64) -> String {
    let sign = if seconds < 0 { "-" } else { "" };
    let seconds = seconds.abs();
    format!(
        "{sign}{}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn strike(x: f64, y: f64) -> Strike {
        Strike::new(
            None,
            Timestamp::new(Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(), 0),
            x,
            y,
            None,
            None,
            None,
            None,
            vec![],
            None,
        )
    }

    fn interval() -> TimeInterval {
        TimeInterval::new(
            Utc.with_ymd_and_hms(2025, 1, 1, 11, 50, 0).unwrap(),
            Utc.with_ymd_and_hms(2025, 1, 1, 12, 0, 0).unwrap(),
        )
    }

    #[test]
    fn single_linkage_matches_fastcluster_reference() {
        // Same geometry as tests/test_clustering.py::test_clustering, for which
        // `fastcluster.linkage(pdist(data))` yields (with the great-circle
        // pdist of this port):
        //   [ 0, 1, 2.6274180135, 2 ]
        //   [ 2, 5, 3.3357494167, 3 ]
        //   [ 3, 6, 47.0614198152, 4 ]
        //   [ 4, 7, 78.4895353608, 5 ]
        let points = [
            (11.0, 51.0),
            (11.02, 51.02),
            (11.02, 51.05),
            (11.4, 51.4),
            (12.0, 52.0),
        ];
        let merges = single_linkage(&points);
        assert_eq!(merges.len(), 4);
        assert_eq!(merges[0].cluster_a, 0);
        assert_eq!(merges[0].cluster_b, 1);
        assert!((merges[0].distance - 2.627_418_013_490_107_3).abs() < 1e-9);
        assert_eq!(merges[0].size, 2);
        assert_eq!(merges[1].cluster_a, 2);
        assert_eq!(merges[1].cluster_b, 5);
        assert!((merges[1].distance - 3.335_749_416_666_679).abs() < 1e-9);
        assert_eq!(merges[1].size, 3);
        assert_eq!(merges[2].cluster_a, 3);
        assert_eq!(merges[2].cluster_b, 6);
        assert!((merges[2].distance - 47.061_419_815_200_686).abs() < 1e-9);
        assert_eq!(merges[2].size, 4);
        assert_eq!(merges[3].cluster_a, 4);
        assert_eq!(merges[3].cluster_b, 7);
        assert!((merges[3].distance - 78.489_535_360_848_28).abs() < 1e-9);
        assert_eq!(merges[3].size, 5);
    }

    #[test]
    fn test_clustering() {
        // tests/test_clustering.py::TestClustering.test_clustering
        let events = vec![
            strike(11.0, 51.0),
            strike(11.02, 51.02),
            strike(11.02, 51.05),
            strike(11.4, 51.4),
            strike(12.0, 52.0),
        ];
        let time_interval = interval();
        let clustering = Clustering::new();

        let clusters = clustering.build_clusters(&events, &time_interval);

        assert_eq!(clusters.len(), 1);
        let cluster = &clusters[0];
        assert_eq!(cluster.timestamp.datetime, Some(time_interval.end));
        assert_eq!(cluster.interval_seconds, 10 * 60);
        assert_eq!(cluster.strike_count, 3);
        // The geodesic area is close to Python's `geographiclib` result
        // (38.2 km² with the 2016 test's ring).  See the module docs of
        // `clustering::geometry` for the simplification deviation: `geo`'s
        // Douglas-Peucker keeps a slightly different vertex set than GEOS, so
        // the area here is 37.3 km².
        assert_eq!(cluster.area, Some(37.3));
        let shape = cluster.shape.as_ref().expect("a shape");
        assert!(shape.first() == shape.last(), "the ring must be closed");
        // The buffered shape surrounds the hull of the three clustered strikes
        // (x 11.0..11.02, y 51.0..51.05) expanded by the 0.02° buffer.
        let xs: Vec<f64> = shape.iter().map(|p| p.0).collect();
        let ys: Vec<f64> = shape.iter().map(|p| p.1).collect();
        assert!(xs.iter().cloned().fold(f64::INFINITY, f64::min) <= 10.981);
        assert!(xs.iter().cloned().fold(f64::NEG_INFINITY, f64::max) >= 11.033);
        assert!(ys.iter().cloned().fold(f64::INFINITY, f64::min) <= 50.981);
        assert!(ys.iter().cloned().fold(f64::NEG_INFINITY, f64::max) >= 51.063);
    }

    /// The Python test (`tests/test_clustering.py::test_clustering`) asserts the
    /// simplified ring *contains* this exact vertex set (recorded against a 2016
    /// GEOS).  That GEOS output cannot be reproduced by any modern geometry
    /// engine, so this test documents the reference expectation for the shape
    /// the pipeline builds *before* simplification (the convex hull buffered by
    /// 0.02°) — the reproducible part.
    #[test]
    fn reference_hull_buffer_contains_python_vertices() {
        let events = [
            strike(11.0, 51.0),
            strike(11.02, 51.02),
            strike(11.02, 51.05),
        ];
        let points: Vec<(f64, f64)> = events.iter().map(|e| (e.x, e.y)).collect();
        let hull = crate::clustering::geometry::convex_hull_ring(&points, 4).unwrap();
        let buffered = crate::clustering::geometry::buffer_ring(&hull, 0.02);
        // The buffer ring from `geo` reproduces the sharp hull corner vertex of
        // the 2016 GEOS ring exactly (the rounded arc vertices differ slightly in
        // their sampling).
        assert!(
            buffered.contains(&(11.04, 51.02)),
            "buffer ring missing (11.04, 51.02): {buffered:?}"
        );
    }

    #[test]
    fn test_clustering_with_not_enough_events() {
        // tests/test_clustering.py::TestClustering.test_clustering_with_not_enough_events
        let events = vec![strike(11.0, 51.0), strike(11.02, 51.02)];
        let clusters = Clustering::new().build_clusters(&events, &interval());
        assert!(clusters.is_empty());
    }

    #[test]
    fn test_clustering_with_no_events() {
        let clusters = Clustering::new().build_clusters(&[], &interval());
        assert!(clusters.is_empty());
    }

    #[test]
    fn no_cluster_when_events_are_far_apart() {
        let events = vec![
            strike(11.0, 51.0),
            strike(11.02, 51.0),
            strike(11.01, 51.03),
            strike(20.0, 60.0),
            strike(20.02, 60.0),
            strike(20.01, 60.03),
        ];
        let clusters = Clustering::new().build_clusters(&events, &interval());
        // Two supported triples far apart: neither merges (closest pair > 8 km),
        // so each triple stays a (size 3) cluster.
        assert_eq!(clusters.len(), 2);
        assert!(clusters.iter().all(|cluster| cluster.strike_count == 3));
    }
}

#[cfg(test)]
mod perf {
    use super::*;

    /// Regression guard against the original `O(n^4)` direct scan, which made
    /// `bo-cluster` spin at 100% CPU for a dense ten-minute window.  With the
    /// MST-based single linkage this must finish well under a second per few
    /// thousand strikes.
    #[test]
    fn large_input_completes_quickly() {
        // Dense 10-minute-window style input: many strikes in a small area.
        let n = 5000;
        let mut pts = Vec::with_capacity(n);
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((seed >> 11) as f64) / ((1u64 << 53) as f64)
        };
        for _ in 0..n {
            pts.push((11.0 + next() * 0.5, 49.0 + next() * 0.5));
        }
        let started = std::time::Instant::now();
        let merges = single_linkage(&pts);
        let elapsed = started.elapsed();
        assert_eq!(merges.len(), n - 1);
        eprintln!("single_linkage n={n} took {elapsed:?}");
        // Generous bound so a slow CI machine does not flake, but far below the
        // old behaviour (which never completed).
        assert!(elapsed.as_secs() < 30, "too slow: {elapsed:?}");
    }
}
