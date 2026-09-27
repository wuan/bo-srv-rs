//! Text, JSON and static HTML renderers for the servicelog statistics.

use std::fmt::Write as _;

use crate::stats::aggregate::{DataAreaBucket, ServiceLogStats, TopEntry};

pub fn render_text(day: &str, stats: &ServiceLogStats) -> String {
    let mut out = String::new();
    out.push_str(&format!("servicelog statistics for {day}\n"));
    out.push_str(&format!("total requests: {}\n", stats.total_requests));
    out.push_str(&format!(
        "  local:  {}  global: {}  region: {}\n",
        stats.local_requests, stats.global_requests, stats.region_requests
    ));
    out.push_str(&format!(
        "  offline: {}  interactive: {}\n",
        stats.offline_requests, stats.interactive_requests
    ));
    out.push_str(&format!(
        "  unknown country: {}  unknown city: {}  unknown version: {}\n",
        stats.unknown_country, stats.unknown_city, stats.unknown_version
    ));

    for (title, entries) in [
        ("top countries", &stats.countries),
        ("top cities", &stats.cities),
        ("top client versions", &stats.versions),
    ] {
        out.push_str(&format!("\n{title}:\n"));
        if entries.is_empty() {
            out.push_str("  (none)\n");
        }
        for entry in entries {
            out.push_str(&format!("  {:>8}  {}\n", entry.count, entry.label));
        }
    }

    out.push_str("\ndata_area distribution (local queries):\n");
    if stats.data_area_distribution.is_empty() {
        out.push_str("  (none)\n");
    }
    let max = stats
        .data_area_distribution
        .iter()
        .map(|b| b.count)
        .max()
        .unwrap_or(0);
    for bucket in &stats.data_area_distribution {
        out.push_str(&format!(
            "  {:>4}  {:>8}  {}\n",
            bucket.data_area,
            bucket.count,
            histogram_bar(bucket.count, max, 40)
        ));
    }

    out.push_str(&format!(
        "\nlocal query locations: {} (offline {}, interactive {})\n\n",
        stats.local_queries.len(),
        stats.offline_requests,
        stats.interactive_requests
    ));
    // The text report includes the ASCII world maps (issue #24): one for the
    // offline queries and one for the interactive queries; `--format map`
    // prints the maps alone.
    out.push_str(&crate::map::render_ascii_maps(stats));
    out
}

/// A `#` bar of `width` columns scaled to `max` (at least one `#` for a
/// non-zero count so no bucket renders as blank).
pub(crate) fn histogram_bar(count: u64, max: u64, width: usize) -> String {
    if count == 0 || max == 0 {
        return String::new();
    }
    let columns = ((count as f64 / max as f64) * width as f64).ceil() as usize;
    "#".repeat(columns.clamp(1, width))
}

/// Render the statistics as pretty-printed JSON.
pub fn render_json(day: &str, stats: &ServiceLogStats) -> String {
    let value = serde_json::json!({
        "day": day,
        "total_requests": stats.total_requests,
        "local_requests": stats.local_requests,
        "global_requests": stats.global_requests,
        "region_requests": stats.region_requests,
        "offline_requests": stats.offline_requests,
        "interactive_requests": stats.interactive_requests,
        "unknown_country": stats.unknown_country,
        "unknown_city": stats.unknown_city,
        "unknown_version": stats.unknown_version,
        "countries": top_entries_json(&stats.countries),
        "cities": top_entries_json(&stats.cities),
        "versions": top_entries_json(&stats.versions),
        "data_area_distribution": stats.data_area_distribution.iter().map(|b| serde_json::json!({
            "data_area": b.data_area,
            "count": b.count,
        })).collect::<Vec<_>>(),
        "local_queries": stats.local_queries.iter().map(|q| serde_json::json!({
            "x": q.x,
            "y": q.y,
            "data_area": q.data_area,
            "grid_baselength": q.grid_baselength,
            "minute_length": q.minute_length,
            "interactive": q.is_interactive(),
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

fn top_entries_json(entries: &[TopEntry]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|e| serde_json::json!({ "label": e.label, "count": e.count }))
        .collect()
}

// --- Static HTML report (issue #28) ----------------------------------------

/// Escape the five XML/HTML metacharacters so arbitrary labels (country, city)
/// can be embedded safely in SVG/HTML.
pub(crate) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Render a "top N" list as HTML table rows (`<tr><td>label</td><td>count</td>`).
fn top_table_rows(entries: &[TopEntry]) -> String {
    if entries.is_empty() {
        return "      <tr><td colspan=\"2\" class=\"empty\">(none)</td></tr>\n".to_string();
    }
    let mut rows = String::new();
    for entry in entries {
        let _ = writeln!(
            rows,
            "      <tr><td>{}</td><td class=\"num\">{}</td></tr>",
            escape_html(&entry.label),
            entry.count
        );
    }
    rows
}

/// Render the `data_area` distribution as HTML table rows with a scaled bar.
fn data_area_rows(buckets: &[DataAreaBucket]) -> String {
    if buckets.is_empty() {
        return "      <tr><td colspan=\"3\" class=\"empty\">(none)</td></tr>\n".to_string();
    }
    let max = buckets.iter().map(|b| b.count).max().unwrap_or(0);
    let mut rows = String::new();
    for bucket in buckets {
        let pct = if max == 0 {
            0.0
        } else {
            bucket.count as f64 / max as f64 * 100.0
        };
        let _ = writeln!(
            rows,
            "      <tr><td class=\"num\">{}</td><td class=\"num\">{}</td>\
             <td><span class=\"bar\" style=\"width:{pct:.1}%\"></span></td></tr>",
            bucket.data_area, bucket.count
        );
    }
    rows
}

/// Render a complete, standalone static HTML report for `day`.
///
/// The document embeds the statistics (totals, top countries/cities/versions,
/// `data_area` distribution) and two separate SVG world maps — one for the
/// background/offline queries and one for the interactive queries — each with a
/// light-gray continent basemap under semi-transparent squares shaded by the
/// per-tile query count (see [`crate::map::render_world_svg`]).  All styling is
/// inline in a `<style>` block, so the file is self-contained and needs no
/// network access.
pub fn render_html(day: &str, stats: &ServiceLogStats) -> String {
    let escape = |value: &str| escape_html(value);
    let offline = stats
        .local_queries
        .iter()
        .filter(|q| q.is_offline())
        .count();
    let interactive = stats.local_queries.len() - offline;

    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    html.push_str("  <meta charset=\"utf-8\">\n");
    html.push_str(&format!(
        "  <title>servicelog statistics for {}</title>\n",
        escape(day)
    ));
    html.push_str("  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n");
    html.push_str("  <style>\n");
    html.push_str(
        "    :root { color-scheme: light; }\n\
         \x20   body { margin: 0; padding: 2rem; background: #ffffff; color: #24313b;\n\
         \x20          font-family: system-ui, -apple-system, Segoe UI, sans-serif; }\n\
         \x20   h1 { font-size: 1.4rem; margin: 0 0 .25rem; }\n\
         \x20   h2 { font-size: 1.05rem; margin: 1.5rem 0 .5rem; color: #2b5d8a; }\n\
         \x20   .sub { color: #5f707d; margin: 0 0 1.5rem; }\n\
         \x20   .cards { display: flex; flex-wrap: wrap; gap: .75rem; margin-bottom: 1rem; }\n\
         \x20   .card { background: #f4f6f8; border: 1px solid #dde1e5; border-radius: 8px;\n\
         \x20           padding: .6rem .9rem; min-width: 8rem; }\n\
         \x20   .card .label { font-size: .72rem; text-transform: uppercase; letter-spacing: .05em;\n\
         \x20                  color: #5f707d; }\n\
         \x20   .card .value { font-size: 1.3rem; font-variant-numeric: tabular-nums; }\n\
         \x20   svg.worldmap { display: block; max-width: 100%; height: auto;\n\
         \x20                 border: 1px solid #dde1e5; border-radius: 8px; background: #f5f6f7; }\n\
         \x20   .maps { display: flex; flex-wrap: wrap; gap: 1rem; }\n\
         \x20   .maps figure { flex: 1 1 26rem; margin: 0; }\n\
         \x20   .maps figcaption { font-size: .85rem; color: #5f707d; margin: .4rem 0 0; }\n\
         \x20   .legend { display: flex; flex-wrap: wrap; align-items: center; gap: .5rem 1rem;\n\
         \x20             margin: .5rem 0 0; font-size: .85rem; color: #5f707d; }\n\
         \x20   .swatch { display: inline-block; width: .8rem; height: .8rem; border-radius: 2px;\n\
         \x20             vertical-align: middle; margin-right: .35rem; }\n\
         \x20   table { border-collapse: collapse; width: 100%; max-width: 34rem; }\n\
         \x20   th, td { text-align: left; padding: .3rem .6rem; border-bottom: 1px solid #e7eaee; }\n\
         \x20   th { color: #5f707d; font-weight: 600; font-size: .8rem; }\n\
         \x20   td.num { text-align: right; font-variant-numeric: tabular-nums; width: 6rem; }\n\
         \x20   td.empty { color: #8a99a5; font-style: italic; }\n\
         \x20   .bar { display: inline-block; height: .7rem; min-width: 1px; background: #2f9e6a;\n\
         \x20          border-radius: 3px; }\n\
         \x20   footer { margin-top: 2rem; color: #8a99a5; font-size: .8rem; }\n\
         \x20 </style>\n",
    );
    html.push_str("</head>\n<body>\n");
    html.push_str(&format!(
        "  <h1>servicelog statistics for {}</h1>\n  <p class=\"sub\">{} requests</p>\n",
        escape(day),
        stats.total_requests
    ));

    // Summary cards.
    html.push_str("  <div class=\"cards\">\n");
    for (label, value) in [
        ("total", stats.total_requests),
        ("local", stats.local_requests),
        ("global", stats.global_requests),
        ("region", stats.region_requests),
        ("offline", stats.offline_requests),
        ("interactive", stats.interactive_requests),
        ("unknown country", stats.unknown_country),
        ("unknown city", stats.unknown_city),
        ("unknown version", stats.unknown_version),
    ] {
        let _ = writeln!(
            html,
            "    <div class=\"card\"><div class=\"label\">{}</div>\
             <div class=\"value\">{value}</div></div>",
            escape(label)
        );
    }
    html.push_str("  </div>\n");

    // World maps: one per query category so the overlays never overlap.
    html.push_str("  <h2>Local query locations</h2>\n");
    html.push_str(&format!(
        "  <p class=\"sub\">{} local query locations (background {}, interactive {}); \
         each square is a 5-degree raster cell, shaded by its query count; the land \
         outline is a coarse orientation aid.</p>\n",
        stats.local_queries.len(),
        offline,
        interactive
    ));
    let (offline_svg, interactive_svg) =
        crate::map::render_world_svg(stats, crate::map::MAP_WIDTH, crate::map::MAP_HEIGHT);
    html.push_str("  <div class=\"maps\">\n");
    let _ = write!(
        html,
        "    <figure>{offline_svg}<figcaption>Background / offline queries \
         (minute_length == 10): {offline}</figcaption></figure>\n\
         \x20   <figure>{interactive_svg}<figcaption>Interactive queries \
         (minute_length &gt; 10): {interactive}</figcaption></figure>\n"
    );
    html.push_str("  </div>\n");
    // Shade legend: the square ramp runs from one query to the densest tile.
    let _ = write!(
        html,
        "  <p class=\"legend\">\
         <span><span class=\"swatch\" style=\"background:{}\"></span>water</span>\
         <span><span class=\"swatch\" style=\"background:{}\"></span>land</span>\
         <span>queries per square:</span>",
        crate::map::svg::WATER_FILL,
        crate::map::svg::LAND_FILL,
    );
    for (index, (r, g, b)) in crate::map::svg::SQUARE_RAMP.iter().enumerate() {
        let _ = write!(
            html,
            "<span class=\"swatch\" style=\"background:#{r:02x}{g:02x}{b:02x}\"></span>"
        );
        if index + 1 == crate::map::svg::SQUARE_RAMP.len() {
            html.push_str("<span>more</span>");
        }
    }
    html.push_str("</p>\n");

    // Top lists.
    for (title, entries) in [
        ("Top countries", &stats.countries),
        ("Top cities", &stats.cities),
        ("Top client versions", &stats.versions),
    ] {
        let _ = write!(
            html,
            "  <h2>{title}</h2>\n  <table>\n    <thead><tr><th>{}</th>\
             <th class=\"num\">requests</th></tr></thead>\n    <tbody>\n{}    </tbody>\n  </table>\n",
            escape(title),
            top_table_rows(entries)
        );
    }

    // data_area distribution.
    html.push_str("  <h2>data_area distribution (local queries)</h2>\n");
    html.push_str(
        "  <table>\n    <thead><tr><th class=\"num\">data_area</th>\
         <th class=\"num\">count</th><th></th></tr></thead>\n    <tbody>\n",
    );
    html.push_str(&data_area_rows(&stats.data_area_distribution));
    html.push_str("    </tbody>\n  </table>\n");

    html.push_str("  <footer>Generated by bo-servicelog-stats (static HTML report).</footer>\n");
    html.push_str("</body>\n</html>\n");
    html
}
