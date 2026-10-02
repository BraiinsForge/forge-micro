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

//! Renders a mining-device machine-state snapshot JSON file as a
//! human-readable Markdown or HTML report.
//!
//! The snapshot is a JSON file downloaded from a device's web dashboard,
//! which puts `/api/info`, `/api/config`, the event log, parsed Prometheus
//! metrics and related sections into one envelope.
//!
//! PDF is not produced here, because a browser's print dialog already
//! renders print-quality PDF from the HTML report.
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
