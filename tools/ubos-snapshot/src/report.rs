//! Builds a generic, renderable report from a parsed [`Snapshot`] and
//! renders it as Markdown or self-contained HTML.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

use serde_json::Value;

use crate::events::{EventCodec, Width};
use crate::snapshot::{CatalogEntry, EventsSection, Meta, MetricEntry, Snapshot};

/// Severity of a report table row, used for HTML row tinting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Informational, no tint.
    Info,
    /// Warning, amber tint.
    Warn,
    /// Error, red tint.
    Error,
}

impl Severity {
    /// Parses a snapshot severity string. Anything other than `"warn"`
    /// or `"error"` is treated as informational.
    #[must_use]
    fn parse(value: &str) -> Severity {
        match value {
            "warn" => Severity::Warn,
            "error" => Severity::Error,
            _ => Severity::Info,
        }
    }

    /// CSS class applied to a table row of this severity.
    #[must_use]
    fn css_class(self) -> &'static str {
        match self {
            Severity::Info => "sev-info",
            Severity::Warn => "sev-warn",
            Severity::Error => "sev-error",
        }
    }
}

/// One row of a [`Item::Table`].
pub struct TableRow {
    /// Cell text, in header order.
    pub cells: Vec<String>,
    /// Optional severity, used only for HTML row tinting.
    pub severity: Option<Severity>,
}

impl TableRow {
    /// Builds a row with no severity tint.
    #[must_use]
    fn plain(cells: Vec<String>) -> TableRow {
        TableRow {
            cells,
            severity: None,
        }
    }
}

/// One piece of content within a [`Section`].
pub enum Item {
    /// A list of label/value pairs, rendered as `**label**: value` lines
    /// in Markdown and as labelled paragraphs in HTML.
    KeyValues(Vec<(String, String)>),
    /// A table with a header row.
    Table {
        /// Column headers.
        headers: Vec<String>,
        /// Data rows.
        rows: Vec<TableRow>,
    },
    /// A standalone line of prose.
    Note(String),
}

/// Outcome of rendering one event's message against the catalog.
enum EventRenderOutcome {
    /// The code was found and its arguments decoded to this message.
    Rendered(String),
    /// The code is not present in the catalog at all.
    UnknownCode,
    /// The code is in the catalog, but its declared widths or the
    /// event's argument payload failed to decode.
    Undecodable,
}

/// One titled group of report content.
pub struct Section {
    /// Section heading.
    pub title: String,
    /// Section content, rendered in order.
    pub items: Vec<Item>,
}

impl Section {
    /// `info` fields surfaced in the device-summary section, as
    /// `(json_key, display_label)` pairs.
    const DEVICE_SUMMARY_FIELDS: &'static [(&'static str, &'static str)] = &[
        ("name", "Name"),
        ("hashrate", "Hashrate"),
        ("uptime", "Uptime"),
        ("temperature", "Temperature"),
        ("fan", "Fan"),
        ("power", "Power"),
        ("voltage", "Voltage"),
        ("psu_fault", "PSU fault"),
        ("running", "Running"),
        ("paused", "Paused"),
    ];

    /// Builds a section whose single item flattens `value` to dotted key
    /// paths and renders them as a two-column table, or a placeholder
    /// note if `value` is absent or empty.
    #[must_use]
    fn from_flattened(title: &str, value: Option<&Value>) -> Section {
        Section {
            title: title.to_string(),
            items: Section::flattened_items(value),
        }
    }

    /// Flattens `value` to dotted key paths and wraps them in a table
    /// item, or a placeholder note if `value` is absent or empty.
    #[must_use]
    fn flattened_items(value: Option<&Value>) -> Vec<Item> {
        let Some(value) = value else {
            return vec![Item::Note("not collected".to_string())];
        };
        let mut pairs = Vec::new();
        value.flatten_paths("", &mut pairs);
        if pairs.is_empty() {
            return vec![Item::Note("empty".to_string())];
        }
        let rows = pairs
            .into_iter()
            .map(|(key, val)| TableRow::plain(vec![key, val]))
            .collect();
        vec![Item::Table {
            headers: vec!["Key".to_string(), "Value".to_string()],
            rows,
        }]
    }

    /// Builds the device-summary section: capture metadata plus a
    /// curated set of top-level `info` fields.
    #[must_use]
    fn device_summary(
        meta: &Meta,
        info: Option<&Value>,
        errors: &BTreeMap<String, String>,
    ) -> Section {
        let mut pairs = Vec::new();
        if let Some(generated_at) = &meta.generated_at {
            pairs.push(("Generated at".to_string(), generated_at.clone()));
        }
        if let Some(hostname) = &meta.hostname {
            pairs.push(("Hostname".to_string(), hostname.clone()));
        }
        if let Some(page_url) = &meta.page_url {
            pairs.push(("Page URL".to_string(), page_url.clone()));
        }
        if let Some(user_agent) = &meta.user_agent {
            pairs.push(("User agent".to_string(), user_agent.clone()));
        }
        match info {
            Some(info) => {
                for (key, label) in Section::DEVICE_SUMMARY_FIELDS {
                    if let Some(value) = info.get(*key) {
                        value.flatten_paths(label, &mut pairs);
                    }
                }
            }
            None => {
                if let Some(message) = errors.get("info") {
                    pairs.push(("Info".to_string(), format!("collection failed: {message}")));
                }
            }
        }
        let items = if pairs.is_empty() {
            vec![Item::Note("not collected".to_string())]
        } else {
            vec![Item::KeyValues(pairs)]
        };
        Section {
            title: "Device summary".to_string(),
            items,
        }
    }

    /// Builds the build-and-versions section: web UI build stamp, the
    /// `info.ota` object, and the full `system_info` section.
    #[must_use]
    fn build_versions(meta: &Meta, info: Option<&Value>, system_info: Option<&Value>) -> Section {
        let mut pairs = Vec::new();
        if let Some(rev) = &meta.ui_build_rev {
            pairs.push(("UI build rev".to_string(), rev.clone()));
        }
        if let Some(time) = &meta.ui_build_time {
            pairs.push(("UI build time".to_string(), time.clone()));
        }
        if let Some(info) = info
            && let Some(ota) = info.get("ota")
        {
            ota.flatten_paths("ota", &mut pairs);
        }
        let mut items = Vec::new();
        if !pairs.is_empty() {
            items.push(Item::KeyValues(pairs));
        }
        items.push(Item::Note("system_info".to_string()));
        items.extend(Section::flattened_items(system_info));
        Section {
            title: "Build & versions".to_string(),
            items,
        }
    }

    /// Builds the network section: the `network` object plus
    /// `info.ip`/`info.mac`.
    #[must_use]
    fn network(network: Option<&Value>, info: Option<&Value>) -> Section {
        let mut items = Section::flattened_items(network);
        let mut pairs = Vec::new();
        if let Some(info) = info {
            if let Some(ip) = info.get("ip") {
                ip.flatten_paths("IP", &mut pairs);
            }
            if let Some(mac) = info.get("mac") {
                mac.flatten_paths("MAC", &mut pairs);
            }
        }
        if !pairs.is_empty() {
            items.push(Item::KeyValues(pairs));
        }
        Section {
            title: "Network".to_string(),
            items,
        }
    }

    /// Builds the system-detail section from five generic sub-sections,
    /// each flattened independently under its own label.
    #[must_use]
    fn system_detail(
        asic: Option<&Value>,
        thermal: Option<&Value>,
        cooling: Option<&Value>,
        statistics: Option<&Value>,
        scoreboard: Option<&Value>,
    ) -> Section {
        let parts: [(&str, Option<&Value>); 5] = [
            ("ASIC", asic),
            ("Thermal", thermal),
            ("Cooling", cooling),
            ("Statistics", statistics),
            ("Scoreboard", scoreboard),
        ];
        let mut items = Vec::new();
        for (label, value) in parts {
            items.push(Item::Note(label.to_string()));
            items.extend(Section::flattened_items(value));
        }
        Section {
            title: "System detail".to_string(),
            items,
        }
    }

    /// Builds the metrics section from the browser-parsed metric
    /// entries: a table of the entries that carried a `name`, and any
    /// `raw` entries listed verbatim after it.
    #[must_use]
    fn metrics(metrics: Option<&[MetricEntry]>, errors: &BTreeMap<String, String>) -> Section {
        let Some(metrics) = metrics else {
            let message = errors.get("metrics").map_or_else(
                || "not collected".to_string(),
                |m| format!("collection failed: {m}"),
            );
            return Section {
                title: "Metrics".to_string(),
                items: vec![Item::Note(message)],
            };
        };
        let mut rows = Vec::new();
        let mut raw_lines = Vec::new();
        for entry in metrics {
            match &entry.name {
                Some(name) => {
                    let timestamp = entry
                        .timestamp_ms
                        .map_or_else(String::new, |ms| ms.to_string());
                    rows.push(TableRow::plain(vec![
                        name.clone(),
                        entry.labels.clone().unwrap_or_default(),
                        entry.value.clone().unwrap_or_default(),
                        timestamp,
                    ]));
                }
                None => {
                    if let Some(raw) = &entry.raw {
                        raw_lines.push(raw.clone());
                    }
                }
            }
        }
        let mut items = Vec::new();
        if rows.is_empty() {
            items.push(Item::Note("no metrics rows".to_string()));
        } else {
            items.push(Item::Table {
                headers: vec![
                    "Name".to_string(),
                    "Labels".to_string(),
                    "Value".to_string(),
                    "Timestamp (ms)".to_string(),
                ],
                rows,
            });
        }
        if !raw_lines.is_empty() {
            items.push(Item::Note(format!(
                "{} unparseable line(s): {}",
                raw_lines.len(),
                raw_lines.join("; ")
            )));
        }
        Section {
            title: "Metrics".to_string(),
            items,
        }
    }

    /// Builds the event-log section: every event decoded against the
    /// catalog, sorted by sequence number descending.
    #[must_use]
    fn event_log(
        events: Option<&EventsSection>,
        catalog: Option<&[CatalogEntry]>,
        errors: &BTreeMap<String, String>,
    ) -> Section {
        let Some(events) = events else {
            let message = errors.get("events").map_or_else(
                || "not collected".to_string(),
                |m| format!("collection failed: {m}"),
            );
            return Section {
                title: "Event log".to_string(),
                items: vec![Item::Note(message)],
            };
        };
        let catalog_map: HashMap<u32, &CatalogEntry> = catalog
            .unwrap_or(&[])
            .iter()
            .map(|entry| (entry.code, entry))
            .collect();
        let mut ordered: Vec<&crate::snapshot::EventRecordJson> = events.events.iter().collect();
        ordered.sort_by_key(|event| std::cmp::Reverse(event.seq));

        let mut unknown_count = 0usize;
        let mut undecodable_count = 0usize;
        let mut rows = Vec::with_capacity(ordered.len());
        for event in ordered {
            let severity = Severity::parse(&event.sev);
            let time = EventCodec::format_time(event.ts);
            let message = match Self::render_event_message(event.code, &event.args, &catalog_map) {
                EventRenderOutcome::Rendered(message) => message,
                EventRenderOutcome::UnknownCode => {
                    unknown_count += 1;
                    format!("event {}", event.code)
                }
                EventRenderOutcome::Undecodable => {
                    undecodable_count += 1;
                    format!("event {}", event.code)
                }
            };
            rows.push(TableRow {
                cells: vec![event.seq.to_string(), time, event.sev.clone(), message],
                severity: Some(severity),
            });
        }
        let total = rows.len();
        let mut note = format!("{total} event(s) total");
        if unknown_count > 0 {
            let _ = write!(note, ", {unknown_count} with unknown codes");
        }
        if undecodable_count > 0 {
            let _ = write!(note, ", {undecodable_count} with undecodable arguments");
        }
        let items = vec![
            Item::Table {
                headers: vec![
                    "Seq".to_string(),
                    "Time".to_string(),
                    "Severity".to_string(),
                    "Message".to_string(),
                ],
                rows,
            },
            Item::Note(note),
        ];
        Section {
            title: "Event log".to_string(),
            items,
        }
    }

    /// Renders one event's message against the catalog, distinguishing
    /// why a fallback is needed: the code is absent from the catalog
    /// entirely, or it is present but its declared widths or the
    /// event's own argument payload fail to decode.
    #[must_use]
    fn render_event_message(
        code: u32,
        args_hex: &str,
        catalog: &HashMap<u32, &CatalogEntry>,
    ) -> EventRenderOutcome {
        let Some(entry) = catalog.get(&code) else {
            return EventRenderOutcome::UnknownCode;
        };
        let widths: Result<Vec<Width>, _> = entry.args.iter().map(|w| Width::parse(w)).collect();
        let Ok(widths) = widths else {
            return EventRenderOutcome::Undecodable;
        };
        let Ok(values) = EventCodec::decode_args(args_hex, &widths) else {
            return EventRenderOutcome::Undecodable;
        };
        EventRenderOutcome::Rendered(EventCodec::format_message(&entry.fmt, &values))
    }

    /// Builds the collection-errors section.
    #[must_use]
    fn collection_errors(errors: &BTreeMap<String, String>) -> Section {
        let items = if errors.is_empty() {
            vec![Item::Note("none".to_string())]
        } else {
            vec![Item::KeyValues(
                errors.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            )]
        };
        Section {
            title: "Collection errors".to_string(),
            items,
        }
    }
}

/// A structured, renderable report built from a [`Snapshot`].
pub struct Report {
    /// Report title, shown as the top-level heading.
    pub title: String,
    /// Report sections, rendered in order.
    pub sections: Vec<Section>,
}

impl Report {
    /// Builds the full report from a parsed snapshot.
    #[must_use]
    pub fn from_snapshot(snapshot: &Snapshot) -> Report {
        let title = snapshot.meta.hostname.clone().map_or_else(
            || "Machine state snapshot".to_string(),
            |hostname| format!("Machine state snapshot: {hostname}"),
        );
        let sections = vec![
            Section::device_summary(&snapshot.meta, snapshot.info.as_ref(), &snapshot.errors),
            Section::build_versions(
                &snapshot.meta,
                snapshot.info.as_ref(),
                snapshot.system_info.as_ref(),
            ),
            Section::network(snapshot.network.as_ref(), snapshot.info.as_ref()),
            Section::from_flattened("Configuration", snapshot.config.as_ref()),
            Section::system_detail(
                snapshot.asic.as_ref(),
                snapshot.thermal.as_ref(),
                snapshot.cooling.as_ref(),
                snapshot.statistics.as_ref(),
                snapshot.scoreboard.as_ref(),
            ),
            Section::metrics(snapshot.metrics.as_deref(), &snapshot.errors),
            Section::event_log(
                snapshot.events.as_ref(),
                snapshot.events_catalog.as_deref(),
                &snapshot.errors,
            ),
            Section::collection_errors(&snapshot.errors),
        ];
        Report { title, sections }
    }

    /// Renders the report as Markdown.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# {}\n", self.title);
        for section in &self.sections {
            let _ = writeln!(out, "## {}\n", section.title);
            for item in &section.items {
                item.write_markdown(&mut out);
            }
        }
        out
    }

    /// Renders the report as a single self-contained HTML document.
    #[must_use]
    pub fn to_html(&self) -> String {
        let mut body = String::new();
        for section in &self.sections {
            let _ = writeln!(body, "<h2>{}</h2>", Report::escape_html(&section.title));
            for item in &section.items {
                item.write_html(&mut body);
            }
        }
        format!(
            "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
             <title>{title}</title>\n<style>{style}</style>\n</head>\n<body>\n\
             <h1>{title}</h1>\n{body}</body>\n</html>\n",
            title = Report::escape_html(&self.title),
            style = STYLE,
        )
    }

    /// Escapes the five HTML-significant characters in `text`.
    #[must_use]
    fn escape_html(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for ch in text.chars() {
            match ch {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&#39;"),
                other => out.push(other),
            }
        }
        out
    }
}

impl Item {
    /// Escapes a Markdown table cell: neutralises the pipe delimiter and
    /// any line breaks. A raw newline or carriage return in a
    /// device-controlled value would otherwise split the cell across
    /// table rows and corrupt the rendered layout.
    #[must_use]
    fn escape_markdown_cell(cell: &str) -> String {
        Item::escape_markdown_breaks(&cell.replace('|', "\\|"))
    }

    /// Neutralises line breaks in a device-controlled value so a raw
    /// newline or carriage return cannot split it across Markdown lines.
    #[must_use]
    fn escape_markdown_breaks(text: &str) -> String {
        text.replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', "\\n")
    }

    /// Appends this item's Markdown rendering to `out`.
    fn write_markdown(&self, out: &mut String) {
        match self {
            Item::KeyValues(pairs) => {
                for (key, value) in pairs {
                    let key = Item::escape_markdown_breaks(key);
                    let value = Item::escape_markdown_breaks(value);
                    let _ = writeln!(out, "**{key}**: {value}\n");
                }
            }
            Item::Table { headers, rows } => {
                if headers.is_empty() {
                    return;
                }
                out.push_str("| ");
                out.push_str(&headers.join(" | "));
                out.push_str(" |\n|");
                for _ in headers {
                    out.push_str(" --- |");
                }
                out.push('\n');
                for row in rows {
                    out.push_str("| ");
                    let cells: Vec<String> = row
                        .cells
                        .iter()
                        .map(|cell| Item::escape_markdown_cell(cell))
                        .collect();
                    out.push_str(&cells.join(" | "));
                    out.push_str(" |\n");
                }
                out.push('\n');
            }
            Item::Note(text) => {
                out.push_str(&Item::escape_markdown_breaks(text));
                out.push_str("\n\n");
            }
        }
    }

    /// Appends this item's HTML rendering to `out`.
    fn write_html(&self, out: &mut String) {
        match self {
            Item::KeyValues(pairs) => {
                for (key, value) in pairs {
                    let _ = writeln!(
                        out,
                        "<p><strong>{}</strong>: {}</p>",
                        Report::escape_html(key),
                        Report::escape_html(value)
                    );
                }
            }
            Item::Table { headers, rows } => {
                if headers.is_empty() {
                    return;
                }
                out.push_str("<table>\n<thead>\n<tr>");
                for header in headers {
                    let _ = write!(out, "<th>{}</th>", Report::escape_html(header));
                }
                out.push_str("</tr>\n</thead>\n<tbody>\n");
                for row in rows {
                    let class = row.severity.map_or(String::new(), |severity| {
                        format!(" class=\"{}\"", severity.css_class())
                    });
                    let _ = write!(out, "<tr{class}>");
                    for cell in &row.cells {
                        let _ = write!(out, "<td>{}</td>", Report::escape_html(cell));
                    }
                    out.push_str("</tr>\n");
                }
                out.push_str("</tbody>\n</table>\n");
            }
            Item::Note(text) => {
                let _ = writeln!(out, "<p class=\"note\">{}</p>", Report::escape_html(text));
            }
        }
    }
}

/// Extension for flattening a JSON value to dotted key paths.
///
/// Implemented on [`Value`] rather than as a free function, so the
/// flattening logic reads as behaviour owned by the value being
/// flattened.
trait FlattenPaths {
    /// Appends `(dotted path, display text)` pairs for every leaf
    /// reachable from `self`, under `prefix`.
    fn flatten_paths(&self, prefix: &str, out: &mut Vec<(String, String)>);
}

impl FlattenPaths for Value {
    fn flatten_paths(&self, prefix: &str, out: &mut Vec<(String, String)>) {
        match self {
            Value::Object(map) => {
                for (key, value) in map {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    value.flatten_paths(&path, out);
                }
            }
            Value::Array(items) => {
                for (index, value) in items.iter().enumerate() {
                    value.flatten_paths(&format!("{prefix}[{index}]"), out);
                }
            }
            leaf => {
                let key = if prefix.is_empty() {
                    "value".to_string()
                } else {
                    prefix.to_string()
                };
                out.push((key, leaf.display_text()));
            }
        }
    }
}

/// Extension for rendering a leaf JSON value as display text.
trait DisplayText {
    /// Renders a scalar value without JSON quoting; falls back to
    /// compact JSON for values this tool does not expect to reach here.
    fn display_text(&self) -> String;
}

impl DisplayText for Value {
    fn display_text(&self) -> String {
        match self {
            Value::Null => "\u{2014}".to_string(),
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::String(value) => value.clone(),
            other => other.to_string(),
        }
    }
}

/// Inline CSS for the HTML report: system font stack, zebra tables, and
/// a background tint on warn/error rows.
const STYLE: &str = "
body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; margin: 2rem; color: #1a1a1a; }
h1 { border-bottom: 2px solid #ccc; padding-bottom: 0.3rem; }
h2 { margin-top: 2rem; border-bottom: 1px solid #ddd; padding-bottom: 0.2rem; }
table { border-collapse: collapse; width: 100%; margin: 0.5rem 0 1rem; }
th, td { text-align: left; padding: 0.3rem 0.6rem; border: 1px solid #ddd; }
th { background: #eee; }
tbody tr:nth-child(even) { background: #f7f7f7; }
tbody tr.sev-warn { background: #fff3cd; }
tbody tr.sev-error { background: #f8d7da; }
p.note { color: #555; font-style: italic; }
";

#[cfg(test)]
#[path = "tests/report.rs"]
mod tests_report;
