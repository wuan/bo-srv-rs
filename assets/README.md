# World basemap asset

`world-110m-land.geojson` is a **simplified, embedded basemap** for the
`bo-servicelog-stats --format html` world map (issue #28).

## Source

Derived from the **Natural Earth** 1:110m "Land" physical vector dataset
(`ne_110m_land`), from the official GeoJSON release in the
[`nvkelso/natural-earth-vector`](https://github.com/nvkelso/natural-earth-vector)
repository:

```
https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_110m_land.geojson
```

Natural Earth data is in the **public domain** — no attribution is required.
The source is noted here purely for provenance.

## Processing

The asset is produced from the source above by
`generate_world_basemap.py`, which applies Douglas-Peucker simplification at
~0.1 degrees and rounds coordinates to 2 decimal places.  Latitudes span the full
range `[-90, 90]` (Antarctica reaches the south pole).

Antarctica is the only landmass that crosses the antimeridian.  It is emitted as
**one** ring that closes along the antimeridian / south-pole map edge
(`[180, -90] -> [-180, -90]`), exactly as in the Natural Earth source, so its
closure lies on the map border and is invisible.  An earlier revision had split
Antarctica into two rings at an interior meridian (~59W), which drew straight
vertical closure segments from the south pole up to the Antarctic peninsula and
rendered as a visible seam in the servicelog world map.  The generator rebuilds
only Antarctica from the source and copies every other landmass ring unchanged,
so the fix stays scoped to the ring that had the bug.

Result: 127 rings, ~4165 points, ~71 KB — detailed enough to embed while still
self-contained, and recognizable at 360x180 SVG and the 72x36 ASCII raster.

The landmass is **only** an orientation aid; it is not a survey-accurate
basemap.
