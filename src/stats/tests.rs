use super::*;

use crate::stats::render::{escape_html, histogram_bar};

/// A sample row copied verbatim from the issue (city padded with tabs).
const ISSUE_SAMPLE: &str = "\
08:44:50.596\tUS\tSun Prairie\t\t\tA\t352\tG\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036\n\
08:44:50.637\tSE\tGothenburg\t\t\tA\t352\tG\t0\t10\t5000\t-1\t0\t4\t13\t5\t0.000\n\
08:44:50.655\tRO\tBucharest\t\t\tA\t352\tG\t0\t10\t5000\t-1\t0\t5\t8\t5\t0.039\n\
08:44:51.106\tDE\tBerlin\t\t\t\tA\t352\tG\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000\n\
08:44:51.238\tDE\tUlm\t\t\t\tA\t352\tG\t0\t10\t5000\t-1\t0\t1\t9\t5\t0.000\n\
08:44:51.277\tIT\tVicenza\t\t\t\tA\t352\tG\t0\t10\t5000\t-1\t0\t2\t9\t5\t0.000\n";

#[test]
fn parses_the_issue_example_rows() {
    let outcome = parse_content(ISSUE_SAMPLE);
    assert_eq!(outcome.malformed, 0);
    assert_eq!(outcome.rows.len(), 6);

    let first = &outcome.rows[0];
    assert_eq!(first.timestamp, "08:44:50.596");
    assert_eq!(first.country.as_deref(), Some("US"));
    assert_eq!(first.city.as_deref(), Some("Sun Prairie"));
    assert_eq!(first.platform, "A");
    assert_eq!(first.version, Some(352));
    assert_eq!(first.query_type, 'G');
    assert!(first.is_grid());
    assert!(!first.is_cluster());
    assert_eq!(first.region, 0);
    assert_eq!(first.grid_baselength, Some(25000));
    assert!(first.is_global());
    assert_eq!(first.x, None);

    let local = &outcome.rows[1];
    assert!(local.is_local());
    assert_eq!(local.x, Some(4));
    assert_eq!(local.y, Some(13));
    assert_eq!(local.data_area, Some(5));
    assert_eq!(local.fill, Some(0.0));
}

#[test]
fn ignores_city_padding_when_counting_fields() {
    // A long city spans more tab blocks; the logical fields are unchanged.
    let line = "08:44:51.106\tDE\tFrankfurt am Main\t\tA\t352\tG\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000";
    let row = parse_row(line).expect("parses");
    assert_eq!(row.city.as_deref(), Some("Frankfurt am Main"));
    assert_eq!(row.x, Some(2));
}

#[test]
fn malformed_lines_are_skipped_not_panicking() {
    let content = "garbage\n08:44:50.596\tUS\n\n";
    let outcome = parse_content(content);
    assert!(outcome.rows.is_empty());
    assert_eq!(outcome.malformed, 2);
}

#[test]
fn dash_placeholders_become_none() {
    // A non-Android client (`-` platform) with unknown country/city and no
    // version: 15 logical fields, city padded with tabs.
    let line = "08:44:50.596\t-\t-\t\t\t\t-\tNone\tG\t0\t10\t0\t0\t0\t-\t-\t-\t0.000";
    let row = parse_row(line).expect("parses");
    assert_eq!(row.country, None);
    assert_eq!(row.city, None);
    assert_eq!(row.platform, "-");
    assert_eq!(row.version, None);
    assert_eq!(row.x, None);
}

/// A legacy 14-field row (no query-type column) is rejected as malformed.
#[test]
fn old_14_field_rows_are_rejected() {
    let line = "08:44:50.596\tUS\tSun Prairie\t\t\tA\t352\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036";
    assert!(parse_row(line).is_none());
}

/// A cluster row (`C`) with `-` grid-only fields parses: `minute_length` is
/// `interval_count * 10`, the grid-only fields are `None`.
#[test]
fn cluster_row_parses_with_dash_grid_only_fields() {
    let line = "08:44:50.596\tDE\tBerlin\t\t\t\tA\t352\tC\t0\t60\t-\t0\t-\t-\t-\t-\t-";
    let row = parse_row(line).expect("parses");
    assert_eq!(row.query_type, 'C');
    assert!(row.is_cluster());
    assert!(!row.is_grid());
    assert_eq!(row.minute_length, 60);
    assert_eq!(row.grid_baselength, None);
    assert_eq!(row.count_threshold, None);
    assert_eq!(row.fill, None);
    assert_eq!(row.region, 0);
    assert!(row.is_global());
}

/// A local cluster row keeps its tile coordinates and parses.
#[test]
fn local_cluster_row_parses() {
    let line = "08:44:50.596\tDE\tBerlin\t\t\t\tI\t42\tC\t5\t20\t-\t-1\t-\t101\t202\t5\t-";
    let row = parse_row(line).expect("parses");
    assert!(row.is_cluster());
    assert!(row.is_local());
    assert_eq!(row.minute_offset, 5);
    assert_eq!(row.minute_length, 20); // interval_count * 10
    assert_eq!(row.region, -1);
    assert_eq!(row.x, Some(101));
    assert_eq!(row.y, Some(202));
    assert_eq!(row.data_area, Some(5));
    assert_eq!(row.fill, None);
}

/// The platform column is opaque to the parser: an iOS row carries `I` and its
/// integer version without any special-casing.
#[test]
fn ios_platform_row_parses() {
    let line = "08:44:51.106\tDE\tBerlin\t\t\t\tI\t42\tG\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000";
    let row = parse_row(line).expect("parses");
    assert_eq!(row.platform, "I");
    assert_eq!(row.version, Some(42));
    assert_eq!(row.country.as_deref(), Some("DE"));
    assert_eq!(row.city.as_deref(), Some("Berlin"));
}

#[test]
fn aggregates_totals_and_flavours() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    assert_eq!(stats.total_requests, 6);
    assert_eq!(stats.grid_requests, 6);
    assert_eq!(stats.cluster_requests, 0);
    assert_eq!(stats.global_requests, 1);
    assert_eq!(stats.local_requests, 5);
    assert_eq!(stats.region_requests, 0);
    assert_eq!(stats.unknown_country, 0);
    assert_eq!(stats.unknown_city, 0);
    assert_eq!(stats.unknown_version, 0);
}

#[test]
fn top_lists_are_ordered_by_count_then_label() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    // DE appears twice; the rest once (ties alphabetical).
    assert_eq!(
        stats.countries[0],
        TopEntry {
            label: "DE".into(),
            count: 2
        }
    );
    let labels: Vec<&str> = stats.countries[1..]
        .iter()
        .map(|e| e.label.as_str())
        .collect();
    assert_eq!(labels, ["IT", "RO", "SE", "US"]);
    // Every city is unique here.
    assert_eq!(stats.cities.len(), 6);
    assert_eq!(stats.cities[0].label, "Berlin");
    // All versions are 352.
    assert_eq!(
        stats.versions,
        vec![TopEntry {
            label: "352".into(),
            count: 6
        }]
    );
}

#[test]
fn top_n_limits_the_lists() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 2);
    assert_eq!(stats.countries.len(), 2);
    assert_eq!(stats.cities.len(), 2);
    assert_eq!(stats.versions.len(), 1);
}

#[test]
fn count_zero_yields_empty_lists_but_keeps_totals() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 0);
    assert_eq!(stats.total_requests, 6);
    assert!(stats.countries.is_empty());
    assert!(stats.cities.is_empty());
    assert!(stats.versions.is_empty());
}

#[test]
fn local_queries_collect_only_local_rows_with_coordinates() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    assert_eq!(stats.local_queries.len(), 5);
    assert_eq!(
        stats.local_queries[0],
        LocalQuery {
            x: 4,
            y: 13,
            data_area: 5,
            grid_baselength: Some(5000),
            minute_length: 10,
            query_type: 'G'
        }
    );
}

#[test]
fn unknown_geo_and_version_are_counted_separately() {
    let content = "08:44:50.596\t-\t-\t\t\t\t-\tNone\tG\t0\t10\t0\t0\t0\t-\t-\t-\t0.000\n";
    let stats = aggregate(&parse_content(content).rows, 10);
    assert_eq!(stats.total_requests, 1);
    assert_eq!(stats.unknown_country, 1);
    assert_eq!(stats.unknown_city, 1);
    assert_eq!(stats.unknown_version, 1);
    assert!(stats.countries.is_empty());
    assert!(stats.cities.is_empty());
    assert!(stats.versions.is_empty());
}

/// A local row builder for the distribution tests.
fn local_row(x: i64, y: i64, data_area: &str) -> String {
    format!(
        "08:00:00.000\tDE\tCity\t\t\t\tA\t352\tG\t0\t60\t5000\t-1\t0\t{x}\t{y}\t{data_area}\t0.000"
    )
}

/// The `data_area` distribution counts every local query, sorted ascending
/// by `data_area`.
#[test]
fn data_area_distribution_counts_and_sorts() {
    let content = [
        local_row(1, 1, "5"),
        local_row(2, 1, "10"),
        local_row(3, 1, "5"),
        local_row(4, 1, "20"),
        local_row(5, 1, "10"),
        local_row(6, 1, "5"),
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);
    assert_eq!(
        stats.data_area_distribution,
        vec![
            DataAreaBucket {
                data_area: 5,
                count: 3
            },
            DataAreaBucket {
                data_area: 10,
                count: 2
            },
            DataAreaBucket {
                data_area: 20,
                count: 1
            },
        ]
    );
}

/// Non-local rows do not contribute to the distribution.
#[test]
fn data_area_distribution_ignores_non_local_rows() {
    let content = "08:00:00.000\tDE\tBerlin\t\t\t\tA\t352\tG\t0\t60\t10000\t0\t0\t-\t-\t-\t0.000\n";
    let stats = aggregate(&parse_content(content).rows, 10);
    assert!(stats.data_area_distribution.is_empty());
}

/// A missing (`-`) data_area counts as the 5-degree minimum, matching the
/// map footprint.
#[test]
fn data_area_distribution_defaults_missing_to_minimum() {
    let content = local_row(1, 1, "-");
    let stats = aggregate(&parse_content(&content).rows, 10);
    assert_eq!(
        stats.data_area_distribution,
        vec![DataAreaBucket {
            data_area: 5,
            count: 1
        }]
    );
}

/// The text report lists the distribution with a scaled bar.
#[test]
fn text_render_includes_data_area_distribution() {
    let content = [
        local_row(1, 1, "5"),
        local_row(2, 1, "5"),
        local_row(3, 1, "10"),
        local_row(4, 1, "5"),
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);
    let text = render_text("2023-11-14", &stats);
    assert!(text.contains("data_area distribution (local queries):"));
    assert!(text.contains("5         3"), "{text}");
    assert!(text.contains("10         1"), "{text}");
    // The largest bucket gets the full-width bar.
    assert!(text.contains("######"), "{text}");
}

/// `histogram_bar` scales to the max and keeps non-zero buckets visible.
#[test]
fn histogram_bar_scaling() {
    assert_eq!(histogram_bar(0, 10, 40), "");
    assert_eq!(histogram_bar(10, 10, 40), "#".repeat(40));
    assert_eq!(histogram_bar(5, 10, 40), "#".repeat(20));
    // A tiny count relative to the max still shows one `#`.
    assert_eq!(histogram_bar(1, 1000, 40), "#");
    // A zero max renders nothing (all buckets zero).
    assert_eq!(histogram_bar(0, 0, 40), "");
}

/// The JSON report carries the distribution as objects.
#[test]
fn json_render_includes_data_area_distribution() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    let value: serde_json::Value =
        serde_json::from_str(&render_json("2023-11-14", &stats)).unwrap();
    assert_eq!(value["data_area_distribution"][0]["data_area"], 5);
    assert_eq!(value["data_area_distribution"][0]["count"], 5);
}

#[test]
fn text_render_contains_totals_and_tops() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    let text = render_text("2023-11-14", &stats);
    assert!(text.contains("servicelog statistics for 2023-11-14"));
    assert!(text.contains("total requests: 6"));
    assert!(text.contains("grid: 6  cluster: 0"), "{text}");
    assert!(text.contains("top countries:"));
    assert!(text.contains("DE"));
    assert!(text.contains("local query locations: 5"));
    // The Berlin row uses `minute_length=60` -> one interactive query; the
    // other four local rows use `minute_length=10` -> offline.
    assert!(text.contains("offline: 4  interactive: 1"));
    // The text report includes separate ASCII world maps for the offline
    // (grid queries with minute_length == 10) and interactive (cluster or
    // other windows) queries.
    assert!(text.contains("offline queries (grid, minute_length == 10):"));
    assert!(text.contains("offline local-query world map (72x36 cells of 5 degrees"));
    assert!(text.contains("interactive queries (cluster or minute_length != 10):"));
    assert!(text.contains("interactive local-query world map (72x36 cells of 5 degrees"));
    assert!(text.contains("4 queries, 4 hits"));
}

#[test]
fn json_render_is_valid_and_structured() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    let json = render_json("2023-11-14", &stats);
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["day"], "2023-11-14");
    assert_eq!(value["total_requests"], 6);
    assert_eq!(value["countries"][0]["label"], "DE");
    assert_eq!(value["countries"][0]["count"], 2);
    assert_eq!(value["local_queries"].as_array().unwrap().len(), 5);
}

/// The HTML report is a standalone document containing the map and tables.
#[test]
fn html_report_is_standalone_and_structured() {
    let outcome = parse_content(ISSUE_SAMPLE);
    let stats = aggregate(&outcome.rows, 10);
    let html = render_html("2023-11-14", &stats);

    assert!(html.starts_with("<!DOCTYPE html>"), "{html}");
    assert!(html.trim_end().ends_with("</html>"));
    assert!(html.contains("<style>"), "styling must be inline");
    assert!(html.contains("servicelog statistics for 2023-11-14"));
    assert!(html.contains("<div class=\"label\">total</div>"));
    // Two separate SVG world maps with the light continent basemap and the
    // background/interactive split.
    assert_eq!(html.matches("<svg").count(), 2, "one map per category");
    assert!(html.contains("class=\"basemap\""));
    assert!(
        html.contains(crate::map::svg::WATER_FILL),
        "very light gray water"
    );
    assert!(html.contains(crate::map::svg::LAND_FILL), "light gray land");
    assert!(html.contains("data-set=\"background (offline)\""));
    assert!(html.contains("data-set=\"interactive\""));
    assert!(
        html.contains(&format!(
            "fill-opacity=\"{}\"",
            crate::map::svg::SQUARE_FILL_OPACITY
        )),
        "transparent squares"
    );
    // The top lists are rendered as tables.
    assert!(html.contains("Top countries"));
    assert!(html.contains("Top cities"));
    assert!(html.contains("Top client versions"));
    assert!(html.contains("data_area distribution (local queries)"));
    // No external resources: the document is self-contained (the only URLs are
    // the SVG namespace declarations, which are not fetched).
    assert!(!html.contains("href="), "{html}");
    assert!(!html.contains("src="), "{html}");
    assert!(!html.contains("<link"), "{html}");
    assert_eq!(
        html.matches("http").count(),
        2,
        "only the two SVG xmlns URLs"
    );
}

/// Labels from the log are HTML-escaped so a crafted city cannot inject
/// markup into the report.
#[test]
fn html_report_escapes_labels() {
    let stats = ServiceLogStats {
        total_requests: 1,
        countries: vec![TopEntry {
            label: "<script>alert(1)</script>".to_string(),
            count: 1,
        }],
        ..ServiceLogStats::default()
    };
    let html = render_html("2023-11-14", &stats);
    assert!(!html.contains("<script>alert"), "{html}");
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
}

/// More than ten top entries switch the table to two label/count column pairs;
/// ten or fewer keep the single pair.
#[test]
fn html_report_uses_column_pairs_beyond_ten_entries() {
    let entries = |n: usize| -> Vec<TopEntry> {
        (0..n)
            .map(|i| TopEntry {
                label: format!("C{i}"),
                count: (n - i) as u64,
            })
            .collect()
    };
    let stats = ServiceLogStats {
        total_requests: 12,
        countries: entries(12),
        cities: entries(4),
        versions: entries(11),
        ..ServiceLogStats::default()
    };
    let html = render_html("2023-11-14", &stats);

    // 12 countries -> two column pairs and all 12 labels are present; the
    // table spreads over the full report width.
    let countries = html
        .split("<h2>Top countries</h2>")
        .nth(1)
        .unwrap()
        .split("</table>")
        .next()
        .unwrap();
    assert!(countries.contains("<table class=\"wide\">"), "{countries}");
    assert!(html.contains("table.wide { max-width: none; }"), "{html}");
    assert_eq!(
        countries.matches("<th class=\"num\">requests</th>").count(),
        2,
        "{countries}"
    );
    for i in 0..12 {
        assert!(countries.contains(&format!("<td>C{i}</td>")), "{countries}");
    }
    // The first ten entries are in the left pair, the last two in the right.
    assert!(
        countries
            .contains("<td>C0</td><td class=\"num\">12</td><td>C10</td><td class=\"num\">2</td>"),
        "{countries}"
    );

    // 11 versions -> also two column pairs; 4 cities -> a single pair.
    let versions = html
        .split("<h2>Top client versions</h2>")
        .nth(1)
        .unwrap()
        .split("</table>")
        .next()
        .unwrap();
    assert_eq!(
        versions.matches("<th class=\"num\">requests</th>").count(),
        2,
        "{versions}"
    );
    let cities = html
        .split("<h2>Top cities</h2>")
        .nth(1)
        .unwrap()
        .split("</table>")
        .next()
        .unwrap();
    assert_eq!(
        cities.matches("<th class=\"num\">requests</th>").count(),
        1,
        "{cities}"
    );
    // A single pair keeps the narrow table.
    assert!(!cities.contains("class=\"wide\""), "{cities}");
}

/// A long list gets one label/count column pair per chunk of ten, so
/// `--top 40` yields four pairs (ten entries each) rather than a second pair
/// with thirty entries.
#[test]
fn html_report_uses_one_column_pair_per_ten_entries() {
    let stats = ServiceLogStats {
        total_requests: 40,
        countries: (0..40)
            .map(|i| TopEntry {
                label: format!("C{i}"),
                count: (40 - i) as u64,
            })
            .collect(),
        ..ServiceLogStats::default()
    };
    let html = render_html("2023-11-14", &stats);
    let countries = html
        .split("<h2>Top countries</h2>")
        .nth(1)
        .unwrap()
        .split("</table>")
        .next()
        .unwrap();
    assert_eq!(
        countries.matches("<th class=\"num\">requests</th>").count(),
        4,
        "{countries}"
    );
    assert_eq!(countries.matches("<tr>").count(), 1 + 10, "{countries}");
    // The first row holds the first entry of every chunk (C0, C10, C20, C30).
    assert!(
        countries.contains(
            "<td>C0</td><td class=\"num\">40</td>\
             <td>C10</td><td class=\"num\">30</td>\
             <td>C20</td><td class=\"num\">20</td>\
             <td>C30</td><td class=\"num\">10</td>"
        ),
        "{countries}"
    );
}

/// `escape_html` covers the five metacharacters.
#[test]
fn escape_html_covers_metacharacters() {
    assert_eq!(
        escape_html("a&b<c>d\"e'f"),
        "a&amp;b&lt;c&gt;d&quot;e&#39;f"
    );
}

#[test]
fn day_is_parsed_from_filename() {
    assert_eq!(
        day_from_filename(Path::new("/var/log/blitzortung/servicelog_2023-11-14")),
        Some("2023-11-14".to_string())
    );
    assert_eq!(day_from_filename(Path::new("/tmp/other.log")), None);
    assert_eq!(day_from_filename(Path::new("/tmp/servicelog_bad")), None);
}

#[test]
fn statistics_for_a_real_file() {
    let dir = std::env::temp_dir().join(format!("bo-stats-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("servicelog_2023-11-14");
    std::fs::write(&path, ISSUE_SAMPLE).unwrap();

    let (outcome, stats) = statistics_for_file(&path, 10).unwrap();
    assert_eq!(outcome.rows.len(), 6);
    assert_eq!(stats.total_requests, 6);
    assert_eq!(day_from_filename(&path).as_deref(), Some("2023-11-14"));

    let report = day_report("2023-11-14".to_string(), &outcome, 10);
    assert_eq!(report.malformed, 0);
    assert_eq!(report.stats.total_requests, 6);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- offline / interactive split ---------------------------------------

/// A local row builder with an explicit `minute_length`.
fn local_row_minutes(x: i64, y: i64, data_area: &str, minute_length: i64) -> String {
    format!(
            "08:00:00.000\tDE\tCity\t\t\t\tA\t352\tG\t0\t{minute_length}\t5000\t-1\t0\t{x}\t{y}\t{data_area}\t0.000"
        )
}

#[test]
fn local_query_classifies_offline_and_interactive() {
    let offline = LocalQuery {
        x: 1,
        y: 1,
        data_area: 5,
        grid_baselength: Some(5000),
        minute_length: 10,
        query_type: 'G',
    };
    assert!(offline.is_grid());
    assert!(!offline.is_cluster());
    assert!(offline.is_offline());
    assert!(!offline.is_interactive());

    for len in [11, 60, 120] {
        let interactive = LocalQuery {
            minute_length: len,
            ..offline
        };
        assert!(interactive.is_interactive(), "minute_length {len}");
        assert!(!interactive.is_offline(), "minute_length {len}");
    }

    // A cluster query is never background: even with a 10-minute window it is
    // interactive.
    let cluster = LocalQuery {
        query_type: 'C',
        ..offline
    };
    assert!(cluster.is_cluster());
    assert!(!cluster.is_grid());
    assert!(!cluster.is_offline(), "cluster with minute_length == 10");
    assert!(cluster.is_interactive(), "cluster with minute_length == 10");
}

/// The tile centre uses the client's requested-tile origin (`x * data_area`),
/// so a tile at `(1, 1)` with `data_area=5` is centred at (7.5, 7.5).
#[test]
fn local_query_center_uses_requested_tile_origin() {
    let query = LocalQuery {
        x: 1,
        y: 1,
        data_area: 5,
        grid_baselength: Some(5000),
        minute_length: 10,
        query_type: 'G',
    };
    assert_eq!(query.center_lon_lat(), (7.5, 7.5));

    let query = LocalQuery {
        x: 3,
        y: 2,
        data_area: 10,
        ..query
    };
    // origin (30, 20) + half a 10-degree tile = (35, 25).
    assert_eq!(query.center_lon_lat(), (35.0, 25.0));
}

/// `aggregate` counts offline (grid, `minute_length == 10`) and interactive
/// (everything else) local requests separately.
#[test]
fn aggregate_counts_offline_and_interactive() {
    let content = [
        local_row_minutes(1, 1, "5", 10),
        local_row_minutes(2, 1, "5", 10),
        local_row_minutes(3, 1, "5", 60),
        local_row_minutes(4, 1, "5", 1440),
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);
    assert_eq!(stats.local_requests, 4);
    assert_eq!(stats.offline_requests, 2);
    assert_eq!(stats.interactive_requests, 2);
}

/// A local cluster row builder with an explicit `minute_length`
/// (`interval_count * 10` in the log).
fn local_cluster_row_minutes(x: i64, y: i64, minute_length: i64) -> String {
    format!(
        "08:00:00.000\tDE\tCity\t\t\t\tA\t352\tC\t0\t{minute_length}\t-\t-1\t-\t{x}\t{y}\t5\t-"
    )
}

/// `aggregate` counts grid (`G`) and cluster (`C`) requests separately.
#[test]
fn aggregate_counts_grid_and_cluster_requests() {
    let content = [
        local_row_minutes(1, 1, "5", 10),        // grid, local
        "08:00:00.001\tUS\tCity\t\t\t\tA\t352\tG\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036".to_string(), // grid, global
        local_cluster_row_minutes(2, 1, 10),    // cluster, local
        "08:00:00.002\tDE\tCity\t\t\t\tA\t352\tC\t0\t60\t-\t0\t-\t-\t-\t-\t-".to_string(), // cluster, global
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);
    assert_eq!(stats.total_requests, 4);
    assert_eq!(stats.grid_requests, 2);
    assert_eq!(stats.cluster_requests, 2);
    assert_eq!(stats.local_requests, 2);
    assert_eq!(stats.global_requests, 2);
}

/// A local **cluster** query with `minute_length == 10` (a single 10-minute
/// cluster interval) is interactive, not background: only grid queries with
/// the 10-minute window count as offline.
#[test]
fn cluster_query_with_ten_minute_window_is_interactive() {
    let content = [
        local_row_minutes(1, 1, "5", 10),     // grid, 10 minutes -> offline
        local_cluster_row_minutes(2, 1, 10), // cluster, 10 minutes -> interactive
        local_cluster_row_minutes(3, 1, 60), // cluster, 60 minutes -> interactive
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);
    assert_eq!(stats.grid_requests, 1);
    assert_eq!(stats.cluster_requests, 2);
    assert_eq!(stats.local_requests, 3);
    assert_eq!(stats.local_queries.len(), 3);
    assert_eq!(stats.offline_requests, 1);
    assert_eq!(stats.interactive_requests, 2);
    assert!(stats.local_queries[0].is_offline());
    assert!(stats.local_queries[1].is_interactive());
    assert_eq!(stats.local_queries[1].query_type, 'C');
    assert_eq!(stats.local_queries[1].minute_length, 10);

    // The maps split accordingly: the cluster query lands on the interactive
    // map, not the offline one.
    let offline =
        crate::map::AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
            q.is_offline()
        });
    let interactive =
        crate::map::AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
            q.is_interactive()
        });
    assert_eq!(offline.queries(), 1);
    assert_eq!(interactive.queries(), 2);
}

/// The two maps separate the offline and interactive queries.
#[test]
fn separate_maps_for_offline_and_interactive() {
    let content = [
        // Requested tile (2, 10), data_area=5 -> origin (10E, 50N) -> col 38, row 28.
        local_row_minutes(2, 10, "5", 10),
        // Interactive at the same tile, so the same cell appears in both.
        local_row_minutes(2, 10, "5", 60),
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);

    let offline =
        crate::map::AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
            q.is_offline()
        });
    let interactive =
        crate::map::AsciiWorldMap::from_local_queries_filtered(&stats.local_queries, |q| {
            q.is_interactive()
        });
    assert_eq!(offline.queries(), 1);
    assert_eq!(offline.count(38, 28), 1);
    assert_eq!(interactive.queries(), 1);
    assert_eq!(interactive.count(38, 28), 1);

    let rendered = crate::map::render_ascii_maps(&stats);
    assert!(rendered.contains("offline local-query world map"));
    assert!(rendered.contains("interactive local-query world map"));
}

/// The JSON report marks each query and reports the split counts.
#[test]
fn json_reports_offline_and_interactive() {
    let content = [
        local_row_minutes(1, 1, "5", 10),
        local_row_minutes(2, 1, "5", 60),
        local_cluster_row_minutes(3, 1, 10),
    ]
    .join("\n");
    let stats = aggregate(&parse_content(&content).rows, 10);
    let value: serde_json::Value =
        serde_json::from_str(&render_json("2023-11-14", &stats)).unwrap();
    assert_eq!(value["total_requests"], 3);
    assert_eq!(value["grid_requests"], 2);
    assert_eq!(value["cluster_requests"], 1);
    assert_eq!(value["offline_requests"], 1);
    assert_eq!(value["interactive_requests"], 2);
    assert_eq!(value["local_queries"][0]["interactive"], false);
    assert_eq!(value["local_queries"][0]["minute_length"], 10);
    assert_eq!(value["local_queries"][0]["query_type"], "G");
    assert_eq!(value["local_queries"][1]["interactive"], true);
    assert_eq!(value["local_queries"][1]["minute_length"], 60);
    // The cluster query with minute_length == 10 is interactive, not offline.
    assert_eq!(value["local_queries"][2]["interactive"], true);
    assert_eq!(value["local_queries"][2]["minute_length"], 10);
    assert_eq!(value["local_queries"][2]["query_type"], "C");
}

// --- query matrix (grid/cluster x local/global) ---------------------------

/// A sample covering all four matrix cells plus a region query, which stays
/// outside the matrix: 2 grid-local, 1 grid-global, 1 grid-region,
/// 3 cluster-local.
fn matrix_sample() -> String {
    [
        local_row_minutes(1, 1, "5", 10),     // grid, local
        local_row_minutes(2, 1, "5", 60),     // grid, local
        "08:00:00.001\tUS\tCity\t\t\t\tA\t352\tG\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036".to_string(), // grid, global
        "08:00:00.002\tFR\tCity\t\t\t\tA\t352\tG\t0\t60\t10000\t3\t0\t-\t-\t-\t0.000".to_string(), // grid, region 3
        local_cluster_row_minutes(3, 1, 10),  // cluster, local
        local_cluster_row_minutes(4, 1, 20),  // cluster, local
        local_cluster_row_minutes(5, 1, 60),  // cluster, local
    ]
    .join("\n")
}

/// `aggregate` fills the grid/cluster x local/global matrix; region queries
/// are counted in `region_requests` but not in any matrix cell.
#[test]
fn aggregate_counts_the_query_matrix() {
    let stats = aggregate(&parse_content(&matrix_sample()).rows, 10);
    assert_eq!(stats.query_matrix.grid_local, 2);
    assert_eq!(stats.query_matrix.grid_global, 1);
    assert_eq!(stats.query_matrix.cluster_local, 3);
    assert_eq!(stats.query_matrix.cluster_global, 0);
    // Row/column/grand sums (region query is outside the matrix).
    assert_eq!(stats.query_matrix.grid_sum(), 3);
    assert_eq!(stats.query_matrix.cluster_sum(), 3);
    assert_eq!(stats.query_matrix.local_sum(), 5);
    assert_eq!(stats.query_matrix.global_sum(), 1);
    assert_eq!(stats.query_matrix.total(), 6);
    // The totals still add up: 4 grid (2 local + 1 global + 1 region) + 3 cluster.
    assert_eq!(stats.total_requests, 7);
    assert_eq!(stats.grid_requests, 4);
    assert_eq!(stats.cluster_requests, 3);
    assert_eq!(stats.local_requests, 5);
    assert_eq!(stats.global_requests, 1);
    assert_eq!(stats.region_requests, 1);
}

/// The text report prints the matrix as an aligned table under the totals.
#[test]
fn text_render_includes_the_query_matrix() {
    let stats = aggregate(&parse_content(&matrix_sample()).rows, 10);
    let text = render_text("2023-11-14", &stats);
    assert!(text.contains("query matrix"), "{text}");
    assert!(
        text.contains(&format!(
            "    {:>8}  {:>12}  {:>12}  {:>8}",
            "", "local query", "global query", "sum"
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "    {:>8}  {:>12}  {:>12}  {:>8}",
            "grid", 2, 1, 3
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "    {:>8}  {:>12}  {:>12}  {:>8}",
            "cluster", 3, 0, 3
        )),
        "{text}"
    );
    assert!(
        text.contains(&format!(
            "    {:>8}  {:>12}  {:>12}  {:>8}",
            "sum", 5, 1, 6
        )),
        "{text}"
    );
}

/// The JSON report carries the matrix as nested objects.
#[test]
fn json_render_includes_the_query_matrix() {
    let stats = aggregate(&parse_content(&matrix_sample()).rows, 10);
    let value: serde_json::Value =
        serde_json::from_str(&render_json("2023-11-14", &stats)).unwrap();
    assert_eq!(value["query_matrix"]["grid"]["local"], 2);
    assert_eq!(value["query_matrix"]["grid"]["global"], 1);
    assert_eq!(value["query_matrix"]["grid"]["sum"], 3);
    assert_eq!(value["query_matrix"]["cluster"]["local"], 3);
    assert_eq!(value["query_matrix"]["cluster"]["global"], 0);
    assert_eq!(value["query_matrix"]["cluster"]["sum"], 3);
    assert_eq!(value["query_matrix"]["sum"]["local"], 5);
    assert_eq!(value["query_matrix"]["sum"]["global"], 1);
    assert_eq!(value["query_matrix"]["sum"]["sum"], 6);
}

/// The HTML report renders the matrix as a table with local/global columns.
#[test]
fn html_report_includes_the_query_matrix() {
    let stats = aggregate(&parse_content(&matrix_sample()).rows, 10);
    let html = render_html("2023-11-14", &stats);
    assert!(html.contains("<h2>Query matrix</h2>"), "{html}");
    assert!(html.contains("<th class=\"num\">local query</th>"), "{html}");
    assert!(html.contains("<th class=\"num\">global query</th>"), "{html}");
    assert!(html.contains("<th class=\"num\">sum</th>"), "{html}");
    assert!(
        html.contains(
            "<tr><td>grid</td><td class=\"num\">2</td><td class=\"num\">1</td>\
             <td class=\"num\">3</td></tr>"
        ),
        "{html}"
    );
    assert!(
        html.contains(
            "<tr><td>cluster</td><td class=\"num\">3</td><td class=\"num\">0</td>\
             <td class=\"num\">3</td></tr>"
        ),
        "{html}"
    );
    assert!(
        html.contains(
            "<tr><td>sum</td><td class=\"num\">5</td><td class=\"num\">1</td>\
             <td class=\"num\">6</td></tr>"
        ),
        "{html}"
    );
}
