//! End-to-end test of the `bo-servicelog-stats` binary: it reads a
//! `servicelog_YYYY-MM-DD` file and prints statistics in the requested format.
//!
//! Cargo exposes the built binary path as `CARGO_BIN_EXE_bo-servicelog-stats`.

use std::path::PathBuf;
use std::process::Command;

/// One servicelog file with the six rows from issue #24.
const SAMPLE: &str = "\
08:44:50.596\tUS\tSun Prairie\t\t\tA\t352\t0\t10\t25000\t0\t0\t-\t-\t-\t0.036
08:44:50.637\tSE\tGothenburg\t\t\tA\t352\t0\t10\t5000\t-1\t0\t4\t13\t5\t0.000
08:44:50.655\tRO\tBucharest\t\t\tA\t352\t0\t10\t5000\t-1\t0\t5\t8\t5\t0.039
08:44:51.106\tDE\tBerlin\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t2\t10\t5\t0.000
08:44:51.238\tDE\tUlm\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t1\t9\t5\t0.000
08:44:51.277\tIT\tVicenza\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t2\t9\t5\t0.000
";

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bo-stats-it-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn binary() -> &'static str {
    env!("CARGO_BIN_EXE_bo-servicelog-stats")
}

#[test]
fn text_report_lists_totals_and_tops() {
    let dir = temp_dir("text");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--top",
            "3",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("servicelog statistics for 2023-11-14"),
        "{stdout}"
    );
    assert!(stdout.contains("total requests: 6"), "{stdout}");
    assert!(
        stdout.contains("local:  5  global: 1  region: 0"),
        "{stdout}"
    );
    assert!(stdout.contains("top countries:"), "{stdout}");
    assert!(stdout.contains("2  DE"), "{stdout}");
    assert!(stdout.contains("local query locations: 5"), "{stdout}");
    assert!(stdout.contains("offline: 4  interactive: 1"), "{stdout}");
    // The default text report now includes the separate offline/interactive
    // ASCII world maps.
    assert!(stdout.contains("offline local-query world map"), "{stdout}");
    assert!(
        stdout.contains("interactive local-query world map"),
        "{stdout}"
    );
    assert!(stdout.contains("+---"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn json_report_is_valid_json() {
    let dir = temp_dir("json");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "json",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(&stdout).expect("valid json");
    assert_eq!(value["day"], "2023-11-14");
    assert_eq!(value["total_requests"], 6);
    assert_eq!(value["countries"][0]["label"], "DE");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Local rows with varied `data_area` values (5, 10, 15, 20).
const DATA_AREA_SAMPLE: &str = "\
08:00:00.000\tDE\tCity\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t1\t1\t5\t0.000\n\
08:00:01.000\tDE\tCity\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t2\t1\t10\t0.000\n\
08:00:02.000\tDE\tCity\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t3\t1\t15\t0.000\n\
08:00:03.000\tDE\tCity\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t4\t1\t20\t0.000\n";

#[test]
fn data_area_distribution_is_reported() {
    let dir = temp_dir("data-area");
    std::fs::write(dir.join("servicelog_2023-11-14"), DATA_AREA_SAMPLE).unwrap();

    // Text output: the distribution section lists each data_area.
    let text = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "text",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(text.status.success());
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(
        stdout.contains("data_area distribution (local queries):"),
        "{stdout}"
    );
    for area in ["5", "10", "15", "20"] {
        assert!(stdout.contains(area), "missing data_area {area}: {stdout}");
    }

    // JSON output: one bucket per data_area, sorted ascending.
    let json = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "json",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(json.status.success());
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8(json.stdout).unwrap()).expect("valid json");
    let buckets = value["data_area_distribution"].as_array().unwrap();
    let areas: Vec<i64> = buckets
        .iter()
        .map(|b| b["data_area"].as_i64().unwrap())
        .collect();
    assert_eq!(areas, [5, 10, 15, 20]);
    assert!(buckets.iter().all(|b| b["count"] == 1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Local rows mixing offline (`minute_length=10`) and interactive (60) queries.
const MIXED_MINUTES_SAMPLE: &str = "\
08:00:00.000\tDE\tCity\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t1\t1\t5\t0.000\n\
08:00:01.000\tDE\tCity\t\t\t\tA\t352\t0\t10\t5000\t-1\t0\t2\t1\t5\t0.000\n\
08:00:02.000\tUS\tCity\t\t\t\tA\t352\t0\t60\t5000\t-1\t0\t-14\t9\t5\t0.000\n";

#[test]
fn offline_and_interactive_are_split_into_separate_maps() {
    let dir = temp_dir("minutes");
    std::fs::write(dir.join("servicelog_2023-11-14"), MIXED_MINUTES_SAMPLE).unwrap();

    // Text: the counts and the two labelled maps.
    let text = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "text",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(text.status.success());
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(stdout.contains("offline: 2  interactive: 1"), "{stdout}");
    assert!(
        stdout.contains("offline queries (minute_length == 10):"),
        "{stdout}"
    );
    assert!(
        stdout.contains("interactive queries (minute_length > 10):"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "offline local-query world map (72x36 cells of 5 degrees, 2 queries, 2 hits)"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "interactive local-query world map (72x36 cells of 5 degrees, 1 queries, 1 hits)"
        ),
        "{stdout}"
    );

    // JSON: the split counts and per-query classification.
    let json = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "json",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(json.status.success());
    let value: serde_json::Value =
        serde_json::from_str(&String::from_utf8(json.stdout).unwrap()).expect("valid json");
    assert_eq!(value["offline_requests"], 2);
    assert_eq!(value["interactive_requests"], 1);
    let queries = value["local_queries"].as_array().unwrap();
    assert_eq!(queries[0]["interactive"], false);
    assert_eq!(queries[1]["interactive"], false);
    assert_eq!(queries[2]["interactive"], true);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn svg_report_contains_one_circle_per_local_query() {
    let dir = temp_dir("svg");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "svg",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.starts_with("<svg"), "{stdout}");
    assert_eq!(stdout.matches("<circle").count(), 5);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ascii_map_report_has_frame_and_markers() {
    let dir = temp_dir("map");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-14",
            "--format",
            "map",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("servicelog local-query maps for 2023-11-14"),
        "{stdout}"
    );
    assert!(stdout.contains("72x36 cells of 5 degrees"), "{stdout}");
    // Separate maps: four offline queries (`minute_length=10`) and the one
    // interactive Berlin query (`minute_length=60`).
    assert!(
        stdout.contains("offline queries (minute_length == 10):"),
        "{stdout}"
    );
    assert!(
        stdout.contains("interactive queries (minute_length > 10):"),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "offline local-query world map (72x36 cells of 5 degrees, 4 queries, 4 hits)"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "interactive local-query world map (72x36 cells of 5 degrees, 1 queries, 1 hits)"
        ),
        "{stdout}"
    );
    // Four offline + one interactive -> five non-space map symbols in total.
    let marks: usize = stdout
        .lines()
        .filter(|l| l.starts_with('|'))
        .map(|l| l.chars().filter(|c| *c != ' ' && *c != '|').count())
        .sum();
    assert_eq!(marks, 5, "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn date_filter_selects_one_day() {
    let dir = temp_dir("date");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
    std::fs::write(dir.join("servicelog_2023-11-15"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args([
            "--dir",
            dir.to_str().unwrap(),
            "--date",
            "2023-11-15",
            "--format",
            "json",
        ])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"day\": \"2023-11-15\""), "{stdout}");
    assert!(!stdout.contains("2023-11-14"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn invalid_format_exits_nonzero() {
    let dir = temp_dir("bad-format");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args(["--dir", dir.to_str().unwrap(), "--format", "xml"])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid --format"), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_directory_exits_nonzero() {
    let output = Command::new(binary())
        .args(["--dir", "/nonexistent/bo-servicelog-stats"])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(!output.status.success());
    let _ = std::fs::remove_dir_all("/nonexistent/bo-servicelog-stats");
}

/// Today's UTC date, matching the tool's default-day resolution.
fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)
        .unwrap()
        .format("%Y-%m-%d")
        .to_string()
}

/// Without `--date`/`--all` the tool reports the current UTC day only.
#[test]
fn defaults_to_todays_file() {
    let dir = temp_dir("today");
    std::fs::write(dir.join(format!("servicelog_{}", today_utc())), SAMPLE).unwrap();
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args(["--dir", dir.to_str().unwrap(), "--format", "json"])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains(&format!("\"day\": \"{}\"", today_utc())),
        "{stdout}"
    );
    assert!(!stdout.contains("2023-11-14"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--all` reports every file in the directory.
#[test]
fn all_reports_every_file() {
    let dir = temp_dir("all");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();
    std::fs::write(dir.join("servicelog_2023-11-15"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args(["--dir", dir.to_str().unwrap(), "--all", "--format", "json"])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("\"day\": \"2023-11-14\""), "{stdout}");
    assert!(stdout.contains("\"day\": \"2023-11-15\""), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// When no file matches the default day, a clear note is printed (exit 0).
#[test]
fn no_data_for_today_prints_a_note() {
    let dir = temp_dir("none");
    std::fs::write(dir.join("servicelog_2023-11-14"), SAMPLE).unwrap();

    let output = Command::new(binary())
        .args(["--dir", dir.to_str().unwrap()])
        .output()
        .expect("run bo-servicelog-stats");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("no servicelog file for"), "{stdout}");
    assert!(stdout.contains("nothing to report"), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}
