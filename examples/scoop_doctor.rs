//! Reference interpreter for `scoop doctor`.
//!
//! Demonstrates the "successful but informational" case: a job that
//! typically exits 0 but whose *findings* are the entire product.
//!
//! Also shows typed `Action`s (Command vs Instruction), specific
//! findings with stable codes, and a generic fallback for findings
//! we haven't classified yet.

use execra::{Action, Context, ExitCode, Finding, Interpreter, InterpreterEvent as Event, Line};
use once_cell::sync::Lazy;
use regex::Regex;

static RE_MISSING_7ZIP: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)7-?zip is (?:missing|not installed)").unwrap());
static RE_LONG_PATHS: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)long paths support is not enabled").unwrap());
static RE_DEV_MODE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)windows developer mode is not enabled").unwrap());
static RE_GENERIC: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(?:Warning|Info|Error): (.+)$").unwrap());
static RE_NO_PROBLEMS: Lazy<Regex> = Lazy::new(|| Regex::new(r"^No problems identified").unwrap());

pub struct ScoopDoctor;

impl ScoopDoctor {
    pub fn new() -> Self {
        Self
    }
}

impl Interpreter for ScoopDoctor {
    fn on_line(&mut self, _ctx: &Context, line: &Line) -> Vec<Event> {
        let l = &line.text;

        // Specific, well-known findings. Stable codes let UIs render
        // custom affordances (an "Install 7-Zip" button, etc.).
        if RE_MISSING_7ZIP.is_match(l) {
            return vec![Event::Finding {
                finding: Finding::recommendation(
                    "scoop.doctor.missing_7zip",
                    "7-Zip is missing; some installers will be slower or fail.",
                )
                .with_action(Action::command(
                    "Install 7-Zip",
                    "scoop",
                    ["install", "7zip"],
                )),
            }];
        }

        if RE_LONG_PATHS.is_match(l) {
            return vec![Event::Finding {
                finding: Finding::warning(
                    "scoop.doctor.long_paths_disabled",
                    "Windows long path support is not enabled.",
                )
                .with_action(Action::instruction(
                    "Enable long paths",
                    "Enable LongPathsEnabled in the registry, or run Scoop's fix script.",
                )),
            }];
        }

        if RE_DEV_MODE.is_match(l) {
            return vec![Event::Finding {
                finding: Finding::recommendation(
                    "scoop.doctor.dev_mode_off",
                    "Developer Mode is off; symlink-based shims may require admin.",
                )
                .with_action(Action::instruction(
                    "Enable Developer Mode",
                    "Enable Developer Mode in Windows Settings.",
                )),
            }];
        }

        // Generic fallback. Catches findings we haven't classified
        // so nothing silently drops on the floor.
        if let Some(c) = RE_GENERIC.captures(l) {
            return vec![Event::Finding {
                finding: Finding::warning("scoop.doctor.generic", c[1].to_string()),
            }];
        }

        if RE_NO_PROBLEMS.is_match(l) {
            return vec![Event::Summary {
                text: "No problems identified".into(),
            }];
        }

        vec![]
    }

    fn on_exit(&mut self, _ctx: &Context, _exit: &ExitCode) -> Vec<Event> {
        vec![]
    }
}
