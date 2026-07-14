//! Renders a mining-device machine-state snapshot JSON file as a
//! human-readable Markdown or HTML report.
//!
//! The snapshot is a JSON file downloaded from a device's web
//! dashboard, capturing `/api/info`, `/api/config`, the event log,
//! parsed Prometheus metrics, and related sections in one envelope.
//! This tool parses that envelope and renders a structured report for
//! offline reading or attaching to a bug report.
//!
//! PDF output is deliberately not implemented: render the HTML report
//! (`ubos-snapshot snapshot.json report.html`) and print it to PDF from
//! a browser's print dialog. Browsers already do print-quality PDF
//! rendering of HTML and CSS; reimplementing that here would just be a
//! worse PDF renderer.
//!
//! # Usage
//!
//! ```text
//! ubos-snapshot <snapshot.json>            # Markdown to stdout
//! ubos-snapshot <snapshot.json> report.md   # Markdown to a file
//! ubos-snapshot <snapshot.json> report.html # self-contained HTML to a file
//! ```

mod events;
mod report;
mod snapshot;

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use report::Report;
use snapshot::Snapshot;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Parses arguments, builds the report, and writes it to stdout or the
/// requested output file.
///
/// # Errors
///
/// Returns an error if arguments are missing or the output path has an
/// unrecognised extension, if the input file cannot be read, if it is
/// not a valid version-1 snapshot, or if writing the output fails.
fn run() -> Result<(), Box<dyn Error>> {
    let mut args = Vec::new();
    for arg in std::env::args_os() {
        args.push(arg.into_string().map_err(|_| NonUtf8ArgError)?);
    }
    let Some(input_path) = args.get(1) else {
        return Err(Box::new(UsageError));
    };

    let text = fs::read_to_string(input_path)?;
    let snapshot = Snapshot::parse(&text)?;
    let report = Report::from_snapshot(&snapshot);

    match args.get(2) {
        None => print!("{}", report.to_markdown()),
        Some(path) if has_extension(path, "md") => fs::write(path, report.to_markdown())?,
        Some(path) if has_extension(path, "html") => fs::write(path, report.to_html())?,
        Some(_) => return Err(Box::new(UsageError)),
    }
    Ok(())
}

/// Whether `path` has the given file extension, case-insensitively.
fn has_extension(path: &str, extension: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|found| found.eq_ignore_ascii_case(extension))
}

/// A command-line argument was not valid UTF-8.
#[derive(Debug)]
struct NonUtf8ArgError;

impl fmt::Display for NonUtf8ArgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "command-line argument is not valid UTF-8")
    }
}

impl Error for NonUtf8ArgError {}

/// Missing or malformed command-line arguments.
#[derive(Debug)]
struct UsageError;

impl fmt::Display for UsageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "usage: ubos-snapshot <snapshot.json> [output.md|output.html]\n\
             \n\
             Renders a machine-state snapshot as a Markdown report (stdout, or\n\
             an output.md file) or a self-contained HTML report (output.html).\n\
             \n\
             PDF is not produced directly: open the HTML report in a browser\n\
             and print it to PDF."
        )
    }
}

impl Error for UsageError {}
