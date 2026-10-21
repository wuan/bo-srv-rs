# Tasks

## 1. Cluster-area overlap predicate

- [ ] 1.1 Add a `rings_overlap` predicate to `src/clustering/geometry.rs` that
      builds `geo::Polygon`s from two closed rings and reports a strictly
      positive intersection area, and verify with unit tests covering
      overlapping rings, disjoint rings and boundary-only contact.

## 2. Merge overlapping clusters during detection

- [ ] 2.1 Refactor `Clustering::build_clusters` in `src/clustering/base.rs` to
      keep each surviving group's member strike indices beside its shape, then
      merge every pair of clusters in the interval whose shapes overlap
      (union the member sets and rebuild the shape with the existing
      hull → buffer → simplify pipeline) until a pass finds no overlap; verify
      with a unit test that two overlapping clusters yield one cluster with the
      summed strike count and an area covering both.
- [ ] 2.2 Verify transitive merging and the non-merge cases with unit tests:
      A overlaps B and B overlaps C but not A yields one cluster; disjoint areas
      and boundary-only contact yield separate clusters; clusters with different
      timestamps are never merged.
- [ ] 2.3 Verify the merged output keeps the existing shape contract and
      ordering with unit tests: one closed ring per cluster, `area` recomputed
      from that ring, and clusters emitted in ascending smallest-member-index
      order.
- [ ] 2.4 Update the clustering module docs and the cluster sections of
      `README.md` and `doc/jsonrpc_methods.md` to state that overlapping
      same-timestamp cluster areas are merged, and check the wording against the
      new tests.

## 3. End-to-end verification

- [ ] 3.1 Add a `MockExecutor` test in `src/cli/cluster_tool.rs` (next to the
      existing `fetch_clusters_*` / `--insert` tests) whose strikes produce two
      overlapping cluster areas, and verify the rendered output and the
      `--insert` path both report one merged cluster per interval.
- [ ] 3.2 Run `cargo test` and the cluster-tool checks and confirm the merged
      path passes.

## Workflow follow-up

- Archive the change after the project's review requirements are satisfied.
