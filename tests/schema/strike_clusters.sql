-- -*- coding: utf8 -*-
--
--   Copyright 2025 Andreas Würl
--
--   Licensed under the Apache License, Version 2.0 (the "License");
--   you may not use this file except in compliance with the License.
--   You may obtain a copy of the License at
--
--       http://www.apache.org/licenses/LICENSE-2.0
--
--   Unless required by applicable law or agreed to in writing, software
--   distributed under the License is distributed on an "AS IS" BASIS,
--   WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
--   See the License for the specific language governing permissions and
--   limitations under the License.
--
-- Canonical schema and indexes for the ``strike_clusters`` table (see the
-- class docstring in ``blitzortung/db/table.py``).
--
-- A cluster is a buffered convex hull of a group of strikes, stored as a
-- geography LineString (shapely encodes a LinearRing with the LineString
-- geometry type code).  ``interval_seconds`` identifies the clustering
-- interval length and ``strike_count`` the number of strikes in the cluster.

CREATE TABLE IF NOT EXISTS strike_clusters (
    id               bigserial,
    "timestamp"      timestamptz,
    interval_seconds SMALLINT,
    geog             GEOGRAPHY(LineString),
    strike_count     INT,
    PRIMARY KEY (id)
);

CREATE INDEX IF NOT EXISTS strike_clusters_timestamp_interval_seconds
    ON strike_clusters USING btree("timestamp", interval_seconds);
CREATE INDEX IF NOT EXISTS strike_clusters_id_timestamp_interval_seconds
    ON strike_clusters USING btree(id, "timestamp", interval_seconds);
CREATE INDEX IF NOT EXISTS strike_clusters_geog
    ON strike_clusters USING gist(geog);
CREATE INDEX IF NOT EXISTS strike_clusters_timestamp_interval_seconds_geog
    ON strike_clusters USING gist("timestamp", interval_seconds, geog);