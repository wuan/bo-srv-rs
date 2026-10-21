# Tasks

## 1. Cluster-area overlap and union helpers

- [x] 1.1 Add a `rings_overlap` predicate to `src/clustering/geometry.rs` that
      builds `geo::Polygon`s from two closed rings and reports a strictly
      positive intersection area, and verify with unit tests covering
      overlapping rings, disjoint rings and boundary-only contact.
- [x] 1.2 Add a `union_rings` helper to `src/clustering/geometry.rs` that
      returns the merged ring for two areas: the union's exterior when the union
      is a single polygon without holes, or "not a single ring" when the union
      has a hole or more than one polygon (the caller then falls back to a
      convex rebuild). Verify with unit tests: overlapping rings union to one
      ring covering both; a union that produces a hole is reported for fallback;
      a numerically empty union is reported for fallback.

## 2. Merge overlapping clusters during detection

- [x] 2.1 Refactor `Clustering::build_clusters` in `src/clustering/base.rs` to
      keep each surviving group's member strike indices beside its shape, then
      merge every pair of clusters in the interval whose shapes overlap by
      unioning their rings (`union_rings`), summing their strike counts, and
      keeping the interval timestamp; when `union_rings` reports the union is not
      a single ring, rebuild the shape from the combined member strikes with the
      existing hull → buffer → simplify pipeline. Repeat until a pass finds no
      overlap. Verify with a unit test that two overlapping clusters yield one
      cluster with the summed strike count and an area that is the union of both
      (covering both).
- [x] 2.2 Verify transitive merging, the non-merge cases and the fallback with
      unit tests: A overlaps B and B overlaps C but not A yields one cluster;
      disjoint areas and boundary-only contact yield separate clusters; clusters
      with different timestamps are never merged; a transitive merge whose union
      would contain a hole yields one cluster with a single covering (convex
      fallback) ring.
- [x] 2.3 Verify the merged output keeps the existing shape contract and
      ordering with unit tests: one closed ring per cluster, `area` recomputed
      from that ring, and clusters emitted in ascending smallest-member-index
      order.
- [x] 2.4 Update the clustering module docs and the cluster sections of
      `README.md` and `doc/jsonrpc_methods.md` to state that overlapping
      same-timestamp cluster areas are merged by polygon union (with the convex
      fallback), and check the wording against the new tests.

## 3. End-to-end verification

- [x] 3.1 Add a `MockExecutor` test in `src/cli/cluster_tool.rs` (next to the
      existing `fetch_clusters_*` / `--insert` tests) whose strikes produce two
      overlapping cluster areas, and verify the rendered output and the
      `--insert` path both report one merged cluster per interval.
- [x] 3.2 Run `cargo test` and the cluster-tool checks and confirm the merged
      path passes.

## Workflow follow-up

- Archive the change after the project's review requirements are satisfied.
