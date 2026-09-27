#!/usr/bin/env python3
"""Regenerate ``assets/world-110m-land.geojson``.

The embedded basemap for the ``bo-servicelog-stats --format html`` world map is
derived from the **Natural Earth** 1:110m "Land" physical vector dataset
(``ne_110m_land``), from the official GeoJSON release in the
``nvkelso/natural-earth-vector`` repository:

    https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_110m_land.geojson

Natural Earth data is in the **public domain**.  The source URL is only noted for
provenance.

Processing
----------

* Douglas-Peucker simplification at ~0.1 degrees (``TOLERANCE``).
* Coordinates rounded to 2 decimal places (``DECIMALS``).
* Antarctica is emitted as **one** ring whose closure runs along the
  antimeridian / south-pole map edge (``[180, -90] -> [-180, -90]``), mirroring
  the Natural Earth source.  No meridian is drawn through the map interior.

The script is **surgical**: every non-Antarctic landmass ring is copied
unchanged from an existing asset (``--base``, default the checked-in file).  Only
Antarctica is rebuilt from the source.  That keeps the diff limited to the ring
that actually had the bug and avoids noise from small simplifier tie-break
differences on unrelated continents.

Background
----------

An earlier revision split Antarctica into two rings at an interior meridian
(~59W).  Closing each half along that meridian drew artificial vertical segments
from the south pole up to the Antarctic peninsula, which rendered as a visible
seam in the servicelog world map.  Antarctica is the only Natural Earth 110m
landmass that crosses the antimeridian, and its closure belongs on the map edge.

Usage::

    python3 assets/generate_world_basemap.py [source.geojson]

With no ``source`` argument the script downloads the official release (needs
network access).  The output is written back to the checked-in asset path.
"""

import json
import math
import sys
import urllib.request

SOURCE_URL = (
    "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/"
    "master/geojson/ne_110m_land.geojson"
)
DEFAULT_ASSET = "assets/world-110m-land.geojson"

TOLERANCE = 0.1  # Douglas-Peucker tolerance in degrees
DECIMALS = 2  # coordinate quantization (2 decimal places)


def _perpendicular_distance(point, start, end):
    """Distance from ``point`` to the line segment ``start``-``end``."""
    px, py = point
    ax, ay = start
    bx, by = end
    dx, dy = bx - ax, by - ay
    length = math.hypot(dx, dy)
    if length == 0.0:
        return math.hypot(px - ax, py - ay)
    return abs(dy * (px - ax) - dx * (py - ay)) / length


def simplify(points, tolerance=TOLERANCE):
    """Iterative Douglas-Peucker simplification of a closed polyline."""
    if len(points) < 3:
        return list(points)
    keep = [False] * len(points)
    keep[0] = keep[-1] = True
    stack = [(0, len(points) - 1)]
    while stack:
        start, end = stack.pop()
        if end <= start + 1:
            continue
        max_distance = -1.0
        index = -1
        for i in range(start + 1, end):
            distance = _perpendicular_distance(points[i], points[start], points[end])
            if distance > max_distance:
                max_distance = distance
                index = i
        if max_distance > tolerance:
            keep[index] = True
            stack.append((start, index))
            stack.append((index, end))
    return [point for point, keep_point in zip(points, keep) if keep_point]


def _round(point):
    return [round(point[0], DECIMALS), round(point[1], DECIMALS)]


def process_ring(ring):
    """Simplify a closed source ring and return a closed, rounded ring."""
    points = list(ring)
    if points and points[0] != points[-1]:
        points.append(points[0])
    simplified = [_round(point) for point in simplify(points)]
    simplified.append(simplified[0])
    return simplified


def is_antarctica(ring):
    """Antarctica reaches the south pole and spans the antimeridian."""
    return any(point[1] <= -84.0 for point in ring) and any(
        abs(point[0]) >= 170.0 for point in ring
    )


def exterior_rings(feature):
    """The exterior rings of a GeoJSON Polygon/MultiPolygon feature."""
    geometry = feature.get("geometry") or {}
    kind = geometry.get("type")
    if kind == "Polygon":
        coordinates = geometry.get("coordinates", [])
        return [coordinates[0]] if coordinates else []
    if kind == "MultiPolygon":
        return [polygon[0] for polygon in geometry.get("coordinates", []) if polygon]
    return []


def find_antarctica(source):
    """Return the single source ring that covers Antarctica."""
    for feature in source["features"]:
        for ring in exterior_rings(feature):
            if is_antarctica(ring):
                return ring
    raise ValueError("no Antarctica ring found in the source dataset")


def build_antarctica_feature(source):
    """Build the corrected, single-ring Antarctica feature."""
    ring = process_ring(find_antarctica(source))
    return {
        "type": "Feature",
        "properties": {"name": "Antarctica"},
        "geometry": {"type": "Polygon", "coordinates": [ring]},
    }


def rebuild(base, source):
    """Replace Antarctica in ``base`` with the corrected source ring."""
    features = []
    inserted = False
    for feature in base["features"]:
        if any(is_antarctica(ring) for ring in exterior_rings(feature)):
            if not inserted:
                features.append(build_antarctica_feature(source))
                inserted = True
            continue  # drop the old (split) Antarctica piece(s)
        features.append(feature)
    if not inserted:
        features.append(build_antarctica_feature(source))
    return {"type": "FeatureCollection", "features": features}


def count_points(collection):
    points = 0
    for feature in collection["features"]:
        points += sum(len(ring) for ring in exterior_rings(feature))
    return points


def main(argv):
    source_path = argv[1] if len(argv) > 1 else None

    if source_path:
        with open(source_path) as handle:
            source = json.load(handle)
    else:
        with urllib.request.urlopen(SOURCE_URL) as response:
            source = json.load(response)

    with open(DEFAULT_ASSET) as handle:
        base = json.load(handle)

    result = rebuild(base, source)
    with open(DEFAULT_ASSET, "w") as handle:
        json.dump(result, handle, separators=(",", ":"))

    print(
        f"wrote {DEFAULT_ASSET}: {len(result['features'])} features, "
        f"{count_points(result)} points"
    )


if __name__ == "__main__":
    main(sys.argv)