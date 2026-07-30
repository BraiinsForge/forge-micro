// Copyright (C) 2026  Braiins Systems s.r.o.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.
//
// Braiins Systems s.r.o. reserves the right to grant any party a license
// to this program, or any part thereof, under any terms, and such a grant
// shall be considered distinct from the grant above.

//! End-to-end test: parses the fixture snapshot and checks the rendered
//! Markdown and HTML reports for the content each section should carry.

use crate::report::Report;
use crate::snapshot::Snapshot;

const FIXTURE: &str = include_str!("fixtures/snapshot.json");

#[test]
fn rejects_unsupported_version() {
    let text = FIXTURE.replacen("\"version\": 1", "\"version\": 2", 1);
    assert!(Snapshot::parse(&text).is_err());
}

#[test]
fn markdown_report_covers_every_section() {
    let snapshot = Snapshot::parse(FIXTURE).expect("fixture is a valid version-1 snapshot");
    let markdown = Report::from_snapshot(&snapshot).to_markdown();

    // Device summary.
    assert!(markdown.contains("Device summary"));
    assert!(markdown.contains("10.0.0.5"));

    // Build & versions.
    assert!(markdown.contains("abc123def456"));

    // Network.
    assert!(markdown.contains("Network"));
    assert!(markdown.contains("aa:bb:cc:dd:ee:ff"));

    // Configuration, flattened to dotted paths.
    assert!(markdown.contains("pools[0].url"));

    // System detail: asic was requested but not collected.
    assert!(markdown.contains("System detail"));
    assert!(markdown.contains("not collected"));

    // Metrics: parsed rows plus the unparseable line noted.
    assert!(markdown.contains("board_fan_rpm"));
    assert!(markdown.contains("board_uptime_seconds"));
    assert!(markdown.contains("unparseable"));
    assert!(markdown.contains("not a valid metric line"));

    // Event log: known code decoded with a negative arg, unknown code
    // falls back to "event <code>", both timestamp formats present, and
    // rows sorted seq descending (seq 2 appears before seq 1).
    assert!(markdown.contains("Thermal limit 9500 reached, delta -5"));
    assert!(markdown.contains("event 999999999"));
    assert!(markdown.contains("42s (pre-sync)"));
    assert!(markdown.contains("2024-07-14 23:33:20 UTC"));
    let pos_seq2 = markdown
        .find("Thermal limit 9500")
        .expect("seq 2's decoded message is present");
    let pos_seq1 = markdown
        .find("event 999999999")
        .expect("seq 1's unknown-code fallback is present");
    assert!(pos_seq2 < pos_seq1, "seq 2 should render before seq 1");

    // Collection errors.
    assert!(markdown.contains("HTTP 500"));
}

#[test]
fn html_report_is_self_contained_and_escaped() {
    let snapshot = Snapshot::parse(FIXTURE).expect("fixture is a valid version-1 snapshot");
    let html = Report::from_snapshot(&snapshot).to_html();

    assert!(html.starts_with("<!doctype html>"));
    assert!(html.contains("<style>"));
    assert!(!html.contains("<script"));

    // Zebra + severity classes and a real <table>.
    assert!(html.contains("<table>"));
    assert!(html.contains("class=\"sev-warn\""));

    // Decoded event message present, HTML-escaped where needed.
    assert!(html.contains("Thermal limit 9500 reached, delta -5"));
    assert!(html.contains("event 999999999"));

    // Pool URL contains no characters that require escaping, but the
    // config table itself must still be present.
    assert!(html.contains("pools[0].url"));
}

#[test]
fn html_report_escapes_device_controlled_strings() {
    // The user agent is device-controlled (captured verbatim from the
    // browser) and stands in for any field that could carry HTML-
    // significant characters end to end into the rendered report.
    let text = FIXTURE.replacen(
        "\"user_agent\": \"Mozilla/5.0\"",
        r#""user_agent": "<script>&\"quote\"</script>""#,
        1,
    );
    let snapshot = Snapshot::parse(&text).expect("fixture with injected user agent parses");
    let html = Report::from_snapshot(&snapshot).to_html();

    assert!(html.contains("&lt;script&gt;"));
    assert!(html.contains("&amp;&quot;quote&quot;"));
    assert!(!html.contains("<script>"));
    assert!(!html.contains("&\"quote\""));
}

#[test]
fn event_log_counts_unknown_width_as_undecodable() {
    // A catalog entry whose declared width does not parse must fall
    // back like an unknown code, but be counted separately: the code
    // itself is known, only its argument shape failed to decode.
    let text = r#"{
        "version": 1,
        "meta": {},
        "events": { "events": [
            { "seq": 1, "ts": 123456789, "sev": "info", "code": 42, "args": "00000000000000000000" }
        ]},
        "events_catalog": [
            { "code": 42, "sev": "info", "fmt": "bad width {}", "args": ["u64"] }
        ]
    }"#;
    let snapshot = Snapshot::parse(text).expect("minimal snapshot with a bad catalog width parses");
    let markdown = Report::from_snapshot(&snapshot).to_markdown();

    assert!(markdown.contains("event 42"));
    assert!(markdown.contains("1 event(s) total"));
    assert!(markdown.contains("1 with undecodable arguments"));
    assert!(!markdown.contains("unknown codes"));
}
