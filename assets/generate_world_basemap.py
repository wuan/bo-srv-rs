#!/usr/bin/env python3
"""Regenerate ``assets/world-110m-land.geojson``.

The embedded basemap for the ``bo-servicelog-stats --format html`` world map is
derived from the **Natural Earth** 1:110m physical vector datasets, from the
official GeoJSON release in the ``nvkelso/natural-earth-vector`` repository:

* ``ne_110m_land``  — the landmass outlines
* ``ne_110m_lakes`` — the lakes punched out of the landmass

    https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_110m_land.geojson
    https://raw.githubusercontent.com/nvkelso/natural-earth-vector/master/geojson/ne_110m_lakes.geojson

Natural Earth data is in the **public domain**.  The source URLs are only noted
for provenance.

Processing
----------

* Douglas-Peucker simplification at ~0.1 degrees (``TOLERANCE``).
* Coordinates rounded to 2 decimal places (``DECIMALS``).
* Antarctica is emitted as **one** ring whose closure runs along the
  antimeridian / south-pole map edge (``[180, -90] -> [-180, -90]``), mirroring
  the Natural Earth source.  No meridian is drawn through the map interior.
* Every lake is emitted as an **interior ring** (a hole) of the landmass
  polygon that contains it, so the land fill is masked and the water background
  shows through.  The Caspian Sea, which Natural Earth already carves as an
  interior ring of the Eurasian landmass, is kept.

The script is **surgical**: every non-Antarctic, non-lake landmass ring is
copied unchanged from an existing asset (the checked-in file).  Only Antarctica
is rebuilt from the land source, and lakes are added as holes.  That keeps the
diff limited to what actually changed and avoids noise from small simplifier
tie-break differences on unrelated continents.

Background
----------

An earlier revision split Antarctica into two rings at an interior meridian
(~59W).  Closing each half along that meridian drew artificial vertical segments
from the south pole up to the Antarctic peninsula, which rendered as a visible
seam in the servicelog world map.  Antarctica is the only Natural Earth 110m
landmass that crosses the antimeridian, and its closure belongs on the map edge.

A later revision dropped all interior rings, so lakes (e.g. the US Great Lakes)
rendered as solid land.  Lakes are now kept as holes.

Usage::

    python3 assets/generate_world_basemap.py [land.geojson] [lakes.geojson]

With no arguments the script downloads the official releases (needs network
access).  The output is written back to the checked-in asset path.
"""

import json
import math
import sys
import urllib.request

LAND_URL = (
    "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/"
    "master/geojson/ne_110m_land.geojson"
)
LAKES_URL = (
    "https://raw.githubusercontent.com/nvkelso/natural-earth-vector/"
    "master/geojson/ne_110m_lakes.geojson"
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


def _point_in_ring(point, ring):
    """Ray-casting point-in-polygon test for a closed ring."""
    x, y = point
    inside = False
    count = len(ring)
    for i in range(count):
        x1, y1 = ring[i]
        x2, y2 = ring[(i + 1) % count]
        if (y1 > y) != (y2 > y):
            if x < (x2 - x1) * (y - y1) / (y2 - y1) + x1:
                inside = not inside
    return inside


def rings(feature):
    """All rings of a GeoJSON Polygon/MultiPolygon feature."""
    geometry = feature.get("geometry") or {}
    kind = geometry.get("type")
    if kind == "Polygon":
        return list(geometry.get("coordinates", []))
    if kind == "MultiPolygon":
        return [ring for polygon in geometry.get("coordinates", []) for ring in polygon]
    return []


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


def is_antarctica(ring):
    """Antarctica reaches the south pole and spans the antimeridian."""
    return any(point[1] <= -84.0 for point in ring) and any(
        abs(point[0]) >= 170.0 for point in ring
    )


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


def _lake_holes(lakes, land_source):
    """Every lake as a simplified, closed interior ring.

    Includes the lakes from ``ne_110m_lakes`` plus any interior rings already
    present in the land source (the Caspian Sea), which the earlier pipeline had
    dropped.
    """
    holes = [process_ring(ring) for feature in lakes["features"] for ring in rings(feature)]
    for feature in land_source["features"]:
        geometry = feature.get("geometry") or {}
        if geometry.get("type") == "Polygon":
            for interior in geometry.get("coordinates", [])[1:]:
                holes.append(process_ring(interior))
    return holes


def _assign_holes(features, holes):
    """Append each hole to the exterior ring of the polygon that contains it."""
    for hole in holes:
        for feature in features:
            geometry = feature.get("geometry") or {}
            coordinates = geometry.get("coordinates")
            polygons = [coordinates] if geometry.get("type") == "Polygon" else coordinates
            if not polygons:
                continue
            for polygon in polygons:
                if polygon and _point_in_ring(hole[0], polygon[0]):
                    polygon.append(hole)
                    break
            else:
                continue
            break
        else:
            raise ValueError(f"no containing landmass for lake hole at {hole[0]}")


def _strip_holes(feature):
    """Return ``feature`` with every interior ring removed (exteriors only)."""
    geometry = dict(feature["geometry"])
    coordinates = geometry.get("coordinates")
    if geometry.get("type") == "Polygon":
        geometry["coordinates"] = [coordinates[0]] if coordinates else []
    elif geometry.get("type") == "MultiPolygon":
        geometry["coordinates"] = [[polygon[0]] for polygon in coordinates if polygon]
    stripped = dict(feature)
    stripped["geometry"] = geometry
    return stripped


def rebuild(base, land_source, lakes):
    """Replace Antarctica and add lake holes to ``base``.

    Existing interior rings are stripped first so the rebuild is idempotent.
    """
    features = []
    inserted = False
    for feature in base["features"]:
        if any(is_antarctica(ring) for ring in exterior_rings(feature)):
            if not inserted:
                features.append(build_antarctica_feature(land_source))
                inserted = True
            continue  # drop the old (split) Antarctica piece(s)
        features.append(_strip_holes(feature))
    if not inserted:
        features.append(build_antarctica_feature(land_source))

    _assign_holes(features, _lake_holes(lakes, land_source))
    return {"type": "FeatureCollection", "features": features}


def count_points(collection):
    points = 0
    for feature in collection["features"]:
        points += sum(len(ring) for ring in rings(feature))
    return points


def _load(path, url):
    if path:
        with open(path) as handle:
            return json.load(handle)
    with urllib.request.urlopen(url) as response:
        return json.load(response)


def main(argv):
    land_path = argv[1] if len(argv) > 1 else None
    lakes_path = argv[2] if len(argv) > 2 else None

    land_source = _load(land_path, LAND_URL)
    lakes = _load(lakes_path, LAKES_URL)

    with open(DEFAULT_ASSET) as handle:
        base = json.load(handle)

    result = rebuild(base, land_source, lakes)
    with open(DEFAULT_ASSET, "w") as handle:
        json.dump(result, handle, separators=(",", ":"))

    def polygon_count(feature):
        geometry = feature["geometry"]
        if geometry["type"] == "Polygon":
            return 1
        return len(geometry["coordinates"])

    holes = sum(len(rings(feature)) - polygon_count(feature)
                for feature in result["features"])
    print(
        f"wrote {DEFAULT_ASSET}: {len(result['features'])} features, "
        f"{count_points(result)} points, {holes} lake holes"
    )


if __name__ == "__main__":
    main(sys.argv)