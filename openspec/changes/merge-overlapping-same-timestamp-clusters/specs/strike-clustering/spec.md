# Spec Delta

## Purpose

Strike clustering turns the strikes of one time interval into the buffered
areas that `bo-cluster` stores in `strike_clusters` and the JSON-RPC cluster
methods return, so overlapping areas for one interval must describe one storm
region.

## ADDED Requirements

### Requirement: Overlapping same-timestamp cluster areas merge

The system SHALL merge every group of strike clusters detected for one interval
whose areas overlap into a single cluster. A merged cluster SHALL keep that
interval's timestamp and interval length, SHALL sum the strike counts of the
clusters it replaces, and its area SHALL cover each area it replaces. Clusters
whose areas do not overlap SHALL remain separate.

#### Scenario: Two overlapping areas become one cluster

- **WHEN** two clusters detected for the same interval have areas that overlap
- **THEN** one cluster is produced with the summed strike count and an area covering both

#### Scenario: Transitive overlap merges into one cluster

- **WHEN** cluster A overlaps cluster B and cluster B overlaps cluster C, but A does not overlap C
- **THEN** a single cluster is produced covering A, B and C

#### Scenario: Disjoint areas stay separate

- **WHEN** two clusters detected for the same interval have areas that do not overlap
- **THEN** both clusters are produced unchanged

#### Scenario: Touching areas do not merge

- **WHEN** two cluster areas meet only along a boundary, sharing no interior
- **THEN** both clusters are produced unchanged

#### Scenario: Overlapping areas from different intervals do not merge

- **WHEN** two clusters have overlapping areas but different interval timestamps
- **THEN** both clusters are produced unchanged

### Requirement: Merged cluster shape

A merged cluster SHALL expose exactly one closed boundary ring, as an unmerged
cluster does, so the cluster stays representable as the single
`GEOGRAPHY(LineString)` stored in `strike_clusters` and returned by the cluster
methods.

#### Scenario: Merged cluster keeps a single closed shape

- **WHEN** two overlapping clusters are merged
- **THEN** the merged cluster has one closed ring whose geodesic area is reported as its area
