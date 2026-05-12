//! Reference interpreter for `scoop install <package>`.
//!
//! Demonstrates: phase tracking, byte progress, multi-line `Notes`
//! collection, known-error classification, and a summary line.
//!
//! Coverage matches what rScoop needs to render install jobs in its UI.

use execra::{Context, ExitCode, Finding, Interpreter, InterpreterEvent as Event, Line, Progress};
use once_cell::sync::Lazy;
use regex::Regex;

static RE_INSTALLING: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Installing '([^']+)'").unwrap());
static RE_DOWNLOADING: Lazy<Regex> = Lazy::new(|| Regex::new(r"Downloading '([^']+)'").unwrap());
static RE_BYTES_MB: Lazy<Regex> = Lazy::new(|| Regex::new(r"([\d.]+) MB / ([\d.]+) MB").unwrap());
static RE_CHECKING_HASH: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)checking hash").unwrap());
static RE_HASH_PASS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^Hash check (?:passed|successful)").unwrap());
static RE_HASH_FAIL: Lazy<Regex> = Lazy::new(|| Regex::new(r"Hash check failed").unwrap());
static RE_EXTRACTING: Lazy<Regex> = Lazy::new(|| Regex::new(r"Extracting (.+)\.\.\.").unwrap());
static RE_LINKING: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Linking ~").unwrap());
static RE_SHIMMING: Lazy<Regex> = Lazy::new(|| Regex::new(r"^Creating shim for").unwrap());
static RE_INSTALLED_OK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"'([^']+)' \(([^)]+)\) was installed successfully").unwrap());
static RE_NO_MANIFEST: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"Couldn't find manifest for '([^']+)'").unwrap());
static RE_ALREADY_INSTALLED: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"is already installed").unwrap());

pub struct ScoopInstall {
    in_notes: bool,
    notes_buf: Vec<String>,
}

impl ScoopInstall {
    pub fn new() -> Self {
        Self {
            in_notes: false,
            notes_buf: Vec::new(),
        }
    }

    fn flush_notes(&mut self) -> Vec<Event> {
        if self.notes_buf.is_empty() {
            return vec![];
        }
        let msg = std::mem::take(&mut self.notes_buf).join("\n");
        self.in_notes = false;
        vec![Event::Finding {
            finding: Finding::info("scoop.notes", msg),
        }]
    }
}

impl Interpreter for ScoopInstall {
    fn on_line(&mut self, _ctx: &Context, line: &Line) -> Vec<Event> {
        let l = &line.text;

        // Multi-line Notes block. Collect until blank line.
        if self.in_notes {
            if l.is_empty() {
                return self.flush_notes();
            }
            if !l.trim_start().starts_with("---") {
                self.notes_buf.push(l.clone());
            }
            return vec![];
        }
        if l == "Notes" {
            self.in_notes = true;
            return vec![];
        }

        // Job-wide label.
        if let Some(c) = RE_INSTALLING.captures(l) {
            return vec![Event::Label {
                text: format!("Installing {}", &c[1]),
            }];
        }

        // Download phase + byte progress.
        if let Some(c) = RE_DOWNLOADING.captures(l) {
            return vec![Event::EnterPhase {
                name: "download".into(),
                label: Some(format!("Downloading {}", &c[1])),
            }];
        }
        if let Some(c) = RE_BYTES_MB.captures(l) {
            let done: f64 = c[1].parse().unwrap_or(0.0);
            let total: f64 = c[2].parse().unwrap_or(0.0);
            return vec![Event::Progress {
                progress: Progress::bytes_mb(done, total),
            }];
        }
        if RE_CHECKING_HASH.is_match(l) {
            return vec![
                Event::UpdatePhase {
                    label: "Verifying download".into(),
                },
                Event::Progress {
                    progress: Progress::indeterminate("verifying"),
                },
            ];
        }
        if RE_HASH_PASS.is_match(l) {
            return vec![Event::ExitPhase];
        }

        // Extract phase.
        if let Some(c) = RE_EXTRACTING.captures(l) {
            return vec![
                Event::EnterPhase {
                    name: "extract".into(),
                    label: Some(format!("Extracting {}", &c[1])),
                },
                Event::Progress {
                    progress: Progress::indeterminate("extracting"),
                },
            ];
        }

        // Link / shim phase.
        if RE_LINKING.is_match(l) {
            return vec![Event::EnterPhase {
                name: "link".into(),
                label: Some("Linking".into()),
            }];
        }
        if RE_SHIMMING.is_match(l) {
            return vec![Event::EnterPhase {
                name: "link".into(),
                label: Some("Creating shims".into()),
            }];
        }

        // Known errors. Exit code still decides success/failure;
        // these enrich the FailureReason.
        if RE_HASH_FAIL.is_match(l) {
            return vec![Event::KnownError {
                code: "scoop.hash_mismatch".into(),
                message: "Downloaded file hash did not match manifest".into(),
            }];
        }
        if let Some(c) = RE_NO_MANIFEST.captures(l) {
            return vec![Event::KnownError {
                code: "scoop.unknown_package".into(),
                message: format!("No manifest for {}", &c[1]),
            }];
        }

        // Summary lines. Never decide success — only enrich it.
        if let Some(c) = RE_INSTALLED_OK.captures(l) {
            return vec![Event::Summary {
                text: format!("Installed {} {}", &c[1], &c[2]),
            }];
        }
        if RE_ALREADY_INSTALLED.is_match(l) {
            return vec![Event::Summary {
                text: "Already installed (no changes)".into(),
            }];
        }

        vec![]
    }

    fn on_exit(&mut self, _ctx: &Context, _exit: &ExitCode) -> Vec<Event> {
        // Process may have exited mid-Notes-block. Flush what we have.
        self.flush_notes()
    }
}
