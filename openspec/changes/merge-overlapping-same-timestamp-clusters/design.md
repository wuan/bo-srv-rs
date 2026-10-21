# Design

## Context

`Clustering::build_clusters` (`src/clustering/base.rs`) agglomerates the strikes
of one interval with single-linkage agglomeration at `distance_limit = 8` km,
keeps the groups with more than two strikes, and builds one shape per group:
convex hull → round to 4 decimals → round-join buffer `0.02°` → Douglas-Peucker
simplify `0.01°` (`src/clustering/geometry.rs`). All shapes share the interval
end as their `timestamp`.

The distance cut looks only at strike-to-strike distance, while the emitted
areas are the *buffered hulls*. Hulls of two groups farther apart than the cut
can still overlap (an elongated or enclosing cluster reaches over a separate
one), and the `0.02°` buffer widens that gap. `bo-cluster` then prints and
`--insert`s both areas, double-counting strikes and sending overlapping
polygons to `get_global_clusters` / `get_local_clusters`.

The stored shape is a single closed ring: the column is
`GEOGRAPHY(LineString)` and `db::map_cluster` decodes it with
`wkb::decode_linestring`. There is no representation for holes or
multi-polygons, and every existing shape is a buffered convex hull.

## Goals / Non-Goals

**Goals:**

- One area per storm region: any two clusters of the same interval whose areas
  overlap become one cluster.
- Preserve the existing shape pipeline, the single-ring invariant and the
  deterministic emission order, so `bo-cluster` and the JSON-RPC responses keep
  their current shape/field contracts.

**Non-Goals:**

- Changing the linkage distance or the buffer/simplify parameters.
- Rewriting clusters already stored in `strike_clusters` (the producer only
  advances to newer intervals; a backfill is a separate change).
- Merging areas across different intervals, or merging at query time in the
  service.
- Any JSON-RPC, CLI-flag or schema change.

## Decisions

### Merge in the clustering library

`Clustering::build_clusters` performs the merge before returning, so the
printed output, the `--insert`ed rows and the `clusters.produced` metric all
agree, and both the CLI and any future caller inherit the behaviour. Merging in
the service would only fix the read path and leave the table wrong.

### Overlap test is positive-area intersection

Two cluster areas overlap when their interiors share area. The predicate builds
`geo::Polygon`s from the closed rings and checks that
`BooleanOps::intersection` has non-zero area; boundary-only contact (a shared
edge or vertex) is not an overlap. The `geo` crate already provides
`BooleanOps` and `unary_union`, so no new dependency is needed.

*Alternative considered:* a bounding-box or centroid test — rejected, it would
merge shapes that do not actually touch.

### Merge by combining member strikes and rebuilding the shape

`build_clusters` keeps each surviving group's member strike indices next to its
shape. A merge unions the member index sets and rebuilds the shape through the
existing hull → buffer → simplify pipeline, then recomputes the geodesic area,
sums `strike_count`, and reuses the interval `timestamp`/`interval_seconds`.

This keeps the invariant that a cluster area is a buffered convex hull and
always yields a single exterior ring, which the `GEOGRAPHY(LineString)` column
and `decode_linestring` require.

*Alternative considered:* unioning the two buffered polygons directly. It
matches "areas merged" literally but can produce a hole or a multi-polygon with
concave seams, which the storage type and the WKB decoder cannot represent, and
it would break the convex-hull shape contract the rest of the pipeline relies
on.

### Iterate to a fixpoint

Rebuilding a merged shape can grow its area into a further overlap, so merging
repeats until a full pass finds no overlapping pair (transitive closure). The
merge is commutative in the member sets, so the result is order-independent even
though pairs are examined in a fixed order.

### Deterministic ordering

After merging, clusters are emitted in ascending order of their smallest member
strike index, the same key `build_clusters` sorts by today, so a `HashMap`-
backed merge cannot reorder the output between runs.

## Risks / Trade-offs

- A merged area is the convex hull of the combined strikes and can be larger
  than the exact union of the two buffered areas → accepted: it upholds the
  single convex-ring invariant, and the spec only requires the merged area to
  cover the areas it replaces.
- Pairwise intersection over the interval's clusters is `O(k²)` per fixpoint
  pass → `k` is small (a handful of clusters per interval), and the harness
  already fits thousands of strikes in memory.
- Numeric robustness of polygon intersection on rounded rings could report a
  sliver for a shared edge → require a strictly positive intersection area so
  boundary-touching clusters stay separate.

## Migration Plan

- No schema or API change; deploy the producer and later intervals are stored
  merged. Previously stored overlapping rows stay until a backfill is run, which
  is out of scope.
- Rollback is reverting the producer build; stored merged rows remain valid
  clusters.

## Open Questions

None that change the specs, the approach or the tasks. Whether to backfill
already-stored overlapping clusters is deferred to a separate change.
