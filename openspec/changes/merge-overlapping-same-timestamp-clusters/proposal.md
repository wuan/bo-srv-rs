# Proposal

## Why

`bo-cluster` detects strike clusters over one time interval, and every cluster
in a run shares that interval's end as its `timestamp`. The single-linkage step
splits groups only by strike-to-strike distance, so two clusters can be farther
apart than `distance_limit` yet have buffered convex-hull areas that overlap
(for example when a large hull encloses a separate group). The tool then emits
and stores both, so one storm cell is reported twice: strikes are
double-counted and the overlapping polygons reach the map clients.

## What Changes

- Merge the strike clusters detected for the same interval whose buffered areas
  overlap into a single cluster, and repeat until no two remaining areas
  overlap (transitive merge).
- A merged cluster keeps the interval `timestamp` and `interval_seconds`, sums
  the `strike_count` of the clusters it replaces, and its shape/area cover every
  area it replaces, built with the existing hull → buffer → simplify pipeline.
- Keep the emission order deterministic: clusters ordered by the smallest
  original strike index of their members, as today.
- No JSON-RPC parameter, response-shape, CLI-flag or database-schema changes:
  `get_global_clusters` / `get_local_clusters` keep returning the stored rows,
  now already merged by the producer.

## Capabilities

### New Capabilities

- `strike-clustering`: detection of strike clusters over a time interval,
  including the merging of overlapping cluster areas that share one timestamp.

### Modified Capabilities

<!-- No existing specs under openspec/specs/ change; this is the first spec for
     the clustering behaviour. -->

## Impact

- `src/clustering/base.rs` — `Clustering::build_clusters` merges overlapping
  clusters before emitting them.
- `src/clustering/geometry.rs` — new cluster-area overlap predicate.
- `src/cli/cluster_tool.rs` — unchanged API; its text/JSON output and `--insert`
  row count simply reflect the merged clusters, and `clusters.produced` /
  `clusters.inserted` metrics follow.
- `README.md` / `doc/jsonrpc_methods.md` — document that overlapping
  same-timestamp cluster areas are merged.
- No `strike_clusters` schema change. Clusters already stored before this change
  are not rewritten (the producer only advances to newer intervals).
