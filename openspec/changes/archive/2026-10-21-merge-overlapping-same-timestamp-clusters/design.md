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
multi-polygons. An unmerged cluster shape is a buffered convex hull; a merged
shape is generally not convex.

## Goals / Non-Goals

**Goals:**

- One area per storm region: any two clusters of the same interval whose areas
  overlap become one cluster whose area is their union.
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
`geo::Polygon`s from the closed rings and checks that `BooleanOps::intersection`
has non-zero area; boundary-only contact (a shared edge or vertex) is not an
overlap. The `geo` crate already provides `BooleanOps`, so no new dependency is
needed.

*Alternative considered:* a bounding-box or centroid test — rejected, it would
merge shapes that do not actually touch.

### Merge by unioning the buffered areas

A merge unions the two cluster polygons with `geo::BooleanOps::union` and sums
their `strike_count`. When the union is a single polygon without holes its
exterior ring is the merged shape. The interval `timestamp` / `interval_seconds`
are reused.

This is the literal "merge the areas" behaviour: the merged boundary follows the
actual overlapping outlines instead of over-approximating them. The stored
`GEOGRAPHY(LineString)` column and `decode_linestring` only need a single closed
ring, which the union exterior provides.

*Alternative considered:* always rebuilding the convex hull of the combined
member strikes — rejected on review: it inflates the merged area well beyond the
union of the two buffered areas. A convex rebuild is kept only as the fallback
below.

### Fall back to a convex rebuild when the union is not a single ring

A union can produce a hole (for example a ring of areas enclosing a void) or, in
principle, more than one polygon. Neither is representable as one closed ring.
In that case the merge rebuilds the shape from the unioned member strike indices
through the existing hull → buffer → simplify pipeline, which always yields a
single convex ring that covers the areas it replaces. `union_rings` in
`src/clustering/geometry.rs` reports this condition (no single hole-free ring)
so `build_clusters` can choose the fallback.

### Iterate to a fixpoint

Rebuilding a merged shape can grow its area into a further overlap, so merging
repeats until a full pass finds no overlapping pair (transitive closure). Union
is commutative in the member sets, so the result is order-independent even
though pairs are examined in a fixed order.

### Deterministic ordering

After merging, clusters are emitted in ascending order of their smallest member
strike index, the same key `build_clusters` sorts by today, so a `HashMap`-
backed merge cannot reorder the output between runs. Member strike indices are
retained alongside each shape because the ordering key and the convex fallback
both need them.

## Risks / Trade-offs

- A merged shape is a polygon union and is generally not convex, so the
  "cluster area is a buffered convex hull" invariant only holds for unmerged
  clusters. Accepted: storage needs a single closed ring, not convexity, and the
  spec only requires the merged area to cover the areas it replaces.
- A union with a hole or multiple rings falls back to the convex hull of the
  combined strikes and can be larger than the exact union → accepted: it keeps
  the single-ring invariant while still covering every replaced area.
- Numeric robustness of polygon intersection on rounded rings could report a
  sliver for a shared edge → require a strictly positive intersection area so
  boundary-touching clusters stay separate; a numerically degenerate union is
  treated like a non-single ring and takes the fallback.
- Pairwise intersection and union over the interval's clusters is `O(k²)` per
  fixpoint pass → `k` is small (a handful of clusters per interval), and the
  harness already fits thousands of strikes in memory.

## Migration Plan

- No schema or API change; deploy the producer and later intervals are stored
  merged. Previously stored overlapping rows stay until a backfill is run, which
  is out of scope. The database content is not rewritten.
- Rollback is reverting the producer build; stored merged rows remain valid
  clusters.

## Open Questions

None that change the specs, the approach or the tasks. Review resolved the three
open points:

1. Merge by polygon union, with a convex rebuild only when the union produces a
   hole (or otherwise is not a single ring) — replaces the earlier
   always-convex-rebuild default.
2. Only the producer changes; existing database content is not affected.
3. Only interiors sharing area merge; boundary-only contact stays separate.

Whether to backfill already-stored overlapping clusters is deferred to a
separate change.
