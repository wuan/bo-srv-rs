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
-- Canonical schema and indexes for the ``strikes`` table (see the class
-- docstring in ``blitzortung/db/table.py``).
--
-- The table is declaratively RANGE-partitioned by ``"timestamp"`` so that
-- retention is an O(1) partition ``DROP`` instead of a bulk ``DELETE`` plus
-- ``VACUUM``.  The service only ever queries the last 24 hours
-- (``MAX_MINUTES_PER_DAY``), so one daily partition per day is enough; keep a
-- couple of days of margin for the delayed importer feeds.
--
-- IMPORTANT: this file is the **fresh-install** schema.  An existing
-- non-partitioned ``strikes`` table cannot be converted by re-applying this
-- file; see the "Converting an existing database" section of
-- ``doc/database_setup.md`` (drop and recreate, or convert in place).
--
-- Keep the index set in sync with the live database (``\di``) and with
-- ``PRODUCTION_INDEXES`` in ``tests/db/test_db.py``; the test suite fails if the
-- index set here drifts from that list.  Proposed optimisations that are NOT
-- deployed live in ``docs/schema/proposed-indexes.sql`` and must be validated
-- against production query plans before they are moved here.
--
-- The statements are idempotent for the fresh/parent objects, but see the
-- partitioning note above for existing tables.

-- PostgreSQL/PostGIS extensions required by the schema.
CREATE EXTENSION IF NOT EXISTS postgis;
-- The ``strikes_timestamp_geog`` index mixes the timestamp with the geography
-- column, which requires the btree_gist extension.
CREATE EXTENSION IF NOT EXISTS btree_gist;

-- Declaratively range-partitioned parent.  The partition key must be part of
-- every unique constraint, hence the ``(id, "timestamp")`` primary key; ``id``
-- is still effectively unique through the sequence, but it can no longer be
-- the sole primary key.
CREATE TABLE IF NOT EXISTS strikes (
    id            bigserial,
    "timestamp"   timestamptz NOT NULL,
    nanoseconds   SMALLINT,
    geog          GEOGRAPHY(Point),
    altitude      SMALLINT,
    region        SMALLINT,
    amplitude     REAL,
    error2d       SMALLINT,
    stationcount  SMALLINT,
    CONSTRAINT strikes_pkey PRIMARY KEY (id, "timestamp")
) PARTITION BY RANGE ("timestamp");

-- Indexes on the partitioned parent are created on every existing partition and
-- on every partition created later, so queries against the parent stay covered.
--
-- Time-range queries (URL de-duplication, histogram) and get_latest_time()
-- (ORDER BY "timestamp" DESC LIMIT 1) use this btree index.
CREATE INDEX IF NOT EXISTS strikes_timestamp ON strikes USING btree("timestamp");

-- Combined region/time-range queries.
CREATE INDEX IF NOT EXISTS strikes_region_timestamp ON strikes USING btree(region, "timestamp");

-- Spatial queries (grid and histogram envelope filters).  As a multicolumn
-- GiST index it can also serve predicates on the geography column alone.
CREATE INDEX IF NOT EXISTS strikes_timestamp_geog ON strikes USING gist("timestamp", geog);

-- ---------------------------------------------------------------------------
-- Partition maintenance
-- ---------------------------------------------------------------------------

-- Create the daily partition for ``p_day`` (UTC) if it does not exist.
CREATE OR REPLACE FUNCTION strikes_create_partition(p_day date)
RETURNS void
LANGUAGE plpgsql
AS $$
BEGIN
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF strikes FOR VALUES FROM (%L) TO (%L)',
        'strikes_p' || to_char(p_day, 'YYYYMMDD'),
        (p_day::timestamp AT TIME ZONE 'UTC'),
        ((p_day + 1)::timestamp AT TIME ZONE 'UTC'));
END $$;

-- Create the current day plus ``p_ahead`` future partitions.  Schedule this
-- (e.g. with pg_cron) before the day rolls over so inserts never hit a missing
-- partition.
CREATE OR REPLACE FUNCTION strikes_ensure_partitions(p_ahead int DEFAULT 2)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    d date;
BEGIN
    FOR d IN
        SELECT (now() AT TIME ZONE 'UTC')::date + i
        FROM generate_series(0, p_ahead) AS i
    LOOP
        PERFORM strikes_create_partition(d);
    END LOOP;
END $$;

-- Drop every daily partition whose day is older than ``now() - p_keep``.
CREATE OR REPLACE FUNCTION strikes_drop_old_partitions(p_keep interval DEFAULT '2 days')
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    r record;
BEGIN
    FOR r IN
        SELECT c.relname
        FROM pg_inherits i
        JOIN pg_class c ON c.oid = i.inhrelid
        JOIN pg_class p ON p.oid = i.inhparent
        WHERE p.relname = 'strikes'
          AND c.relname LIKE 'strikes_p%'
          AND c.relname < 'strikes_p' || to_char((now() AT TIME ZONE 'UTC') - p_keep, 'YYYYMMDD')
    LOOP
        EXECUTE format('DROP TABLE %I', r.relname);
    END LOOP;
END $$;

-- ---------------------------------------------------------------------------
-- Bootstrap partitions
-- ---------------------------------------------------------------------------

-- Cover the immediate window so the testcontainer (and a fresh install) can
-- insert right away.  Production should run ``strikes_ensure_partitions`` on a
-- schedule instead.
SELECT strikes_create_partition((now() AT TIME ZONE 'UTC')::date - 1);
SELECT strikes_create_partition((now() AT TIME ZONE 'UTC')::date);
SELECT strikes_create_partition((now() AT TIME ZONE 'UTC')::date + 1);
SELECT strikes_create_partition((now() AT TIME ZONE 'UTC')::date + 2);