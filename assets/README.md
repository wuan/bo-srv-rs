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

The asset is produced from the source above with Douglas-Peucker simplification
at ~0.5 degrees, quantizing coordinates to 0.1 degrees, and covering the full
latitude range `[-90, 90]` (Antarctica reaches the south pole).  Rings that cross
the antimeridian are split so no SVG path draws a horizontal streak across the
map.

Result: ~126 rings, ~1800 points, ~34 KB — coarse enough to embed but still
recognizable at 360x180 SVG and the 72x36 ASCII raster.

The landmass is **only** an orientation aid; it is not a survey-accurate
basemap.