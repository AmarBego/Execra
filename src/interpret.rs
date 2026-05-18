//! Optional toolkit for **table-driven interpreters**. Feature: `interpret`.
//!
//! Most CLI wrappers end up writing the same shape of [`Interpreter`]: a list
//! of regexes, and for each match a typed [`InterpreterEvent`] with the
//! occasional `$1` capture spliced into a message. [`RuleInterpreter`] is that
//! shape, factored out — you supply a [`Rule`] table (via the [`rules!`] macro)
//! and, optionally, a [`PhaseModel`], a [`NotesConfig`], and a
//! [`FallbackPolicy`]; it handles the dispatch, the flat sequential-phase
//! bookkeeping, multi-line note collection, and the on-exit error fallback.
//!
//! This is a *native-Rust helper*, not a data DSL — patterns and templates are
//! still ordinary Rust string literals in your source. It does not make Execra
//! a catalogue of built-in interpreters: there is nothing tool-specific here.
//! If your CLI doesn't fit the table shape, hand-write [`Interpreter`] as
//! before; the two compose freely (one wraps the other).
//!
//! ```ignore
//! use execra::interpret::{RuleInterpreter, FallbackPolicy, PhaseModel};
//! use execra::rules;
//!
//! struct MyPhases;
//! impl PhaseModel for MyPhases {
//!     fn range(&self, name: &str) -> Option<(f32, f32)> {
//!         match name {
//!             "download" => Some((0.05, 0.50)),
//!             "extract"  => Some((0.50, 0.95)),
//!             _ => None,
//!         }
//!     }
//! }
//!
//! fn interpreter() -> impl execra::Interpreter {
//!     RuleInterpreter::new(rules![
//!         benign,                                  r"^done\.?$";
//!         known,  "tool.hash", "Hash mismatch",    r"(?i)hash check failed";
//!         enter_phase, "download", "Downloading $1", r"^Downloading '([^']+)'";
//!         byte_progress_mb,                        r"([\d.]+)\s*MB\s*/\s*([\d.]+)\s*MB";
//!         summary, "Installed $1",                  r"'([^']+)' was installed";
//!     ], MyPhases)
//!     .notes("Notes", "tool.notes")
//!     .fallback(FallbackPolicy::default().code("tool.command_error"))
//! }
//! ```

use regex::{Captures, Regex};

use crate::interpreter::{Context, Interpreter, InterpreterEvent, Line};
use crate::outcome::ExitCode;
use crate::{Finding, Progress, Stream};

/// A single line-classification rule: a compiled regex plus the typed event
/// to emit when it matches. Build these with the [`rules!`] macro rather than
/// by hand.
pub struct Rule {
    regex: Regex,
    action: RuleAction,
}

/// What a matched [`Rule`] emits. Template strings (`template`, the phase
/// label, the progress hint) support `$1`, `$2`, … capture substitution via
/// [`render`].
pub enum RuleAction {
    /// Matched and intentionally silent — short-circuits later rules so
    /// benign noise can't fall through to a generic error catch-all.
    Benign,
    /// Indeterminate progress with a static hint label.
    Progress(String),
    /// Enter a phase. With a [`PhaseModel`], [`RuleInterpreter`] auto-exits any
    /// currently-open phase first (Execra pipelines are typically sequential,
    /// not nested) and emits boundary `Progress` so the bar advances
    /// continuously across the pipeline instead of resetting per phase.
    EnterPhase {
        name: String,
        label_template: String,
    },
    /// Non-fatal warning.
    Warning { code: String, template: String },
    /// Classified error. Emitted once; suppresses the [`FallbackPolicy`]
    /// on-exit synthesis so the same failure never fires twice.
    KnownError { code: String, template: String },
    /// Summary line — attached at finalization in place of a generic
    /// "completed successfully".
    Summary { template: String },
    /// Determinate byte progress. The regex MUST capture `$1 = done_mb` and
    /// `$2 = total_mb` as decimal numbers; unparseable captures degrade
    /// silently to no event. Scaled into the active phase's slice when a
    /// [`PhaseModel`] is present.
    ByteProgressMb,
}

impl Rule {
    /// Compile `pattern` and pair it with `action`. Panics on an invalid
    /// regex — patterns are static program text, so a bad one is a bug, not a
    /// runtime condition.
    pub fn new(pattern: &str, action: RuleAction) -> Self {
        Self {
            regex: Regex::new(pattern)
                .unwrap_or_else(|e| panic!("invalid interpret rule regex {pattern:?}: {e}")),
            action,
        }
    }
}

/// Build a `Vec<`[`Rule`]`>` declaratively. Rules are separated by `;`;
/// message/label/hint arguments are `&str` and may interpolate captures with
/// `$1`, `$2`, … Order matters: first match wins, so put benign/specific
/// rules ahead of generic catch-alls.
///
/// ```ignore
/// rules![
///     benign,                          r"^done\.?$";
///     progress,    "removing",         r"^Removing\b";
///     enter_phase, "download", "DL $1", r"^Downloading '([^']+)'";
///     warning,     "x.held", "$1 held", r"'([^']+)' is held";
///     known,       "x.hash", "bad hash", r"(?i)hash check failed";
///     summary,     "Installed $1",      r"'([^']+)' was installed";
///     byte_progress_mb,                 r"([\d.]+) MB / ([\d.]+) MB";
/// ]
/// ```
#[macro_export]
macro_rules! rules {
    (@one benign, $pat:expr) => {
        $crate::interpret::Rule::new($pat, $crate::interpret::RuleAction::Benign)
    };
    (@one progress, $label:expr, $pat:expr) => {
        $crate::interpret::Rule::new(
            $pat,
            $crate::interpret::RuleAction::Progress(::std::string::ToString::to_string($label)),
        )
    };
    (@one enter_phase, $name:expr, $label:expr, $pat:expr) => {
        $crate::interpret::Rule::new(
            $pat,
            $crate::interpret::RuleAction::EnterPhase {
                name: ::std::string::ToString::to_string($name),
                label_template: ::std::string::ToString::to_string($label),
            },
        )
    };
    (@one warning, $code:expr, $msg:expr, $pat:expr) => {
        $crate::interpret::Rule::new(
            $pat,
            $crate::interpret::RuleAction::Warning {
                code: ::std::string::ToString::to_string($code),
                template: ::std::string::ToString::to_string($msg),
            },
        )
    };
    (@one known, $code:expr, $msg:expr, $pat:expr) => {
        $crate::interpret::Rule::new(
            $pat,
            $crate::interpret::RuleAction::KnownError {
                code: ::std::string::ToString::to_string($code),
                template: ::std::string::ToString::to_string($msg),
            },
        )
    };
    (@one summary, $msg:expr, $pat:expr) => {
        $crate::interpret::Rule::new(
            $pat,
            $crate::interpret::RuleAction::Summary {
                template: ::std::string::ToString::to_string($msg),
            },
        )
    };
    (@one byte_progress_mb, $pat:expr) => {
        $crate::interpret::Rule::new($pat, $crate::interpret::RuleAction::ByteProgressMb)
    };
    ($($kind:ident $(, $arg:expr)+);+ $(;)?) => {
        ::std::vec![ $( $crate::rules!(@one $kind $(, $arg)+) ),+ ]
    };
}

/// Substitute `$1`, `$2`, … in `template` with regex captures. Missing
/// captures collapse to empty; a literal `$` not followed by a digit is
/// preserved.
pub fn render(template: &str, caps: &Captures) -> String {
    let mut out = String::with_capacity(template.len());
    let mut chars = template.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '$' {
            out.push(c);
            continue;
        }
        let mut digits = String::new();
        while let Some(&d) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(d);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            out.push('$');
            continue;
        }
        if let Ok(idx) = digits.parse::<usize>() {
            if let Some(m) = caps.get(idx) {
                out.push_str(m.as_str());
            }
        }
    }
    out
}

/// Maps a phase name to the fraction slice `[start, end]` (each in `0..=1`)
/// it occupies in overall job progress. [`RuleInterpreter`] uses this to emit
/// continuous boundary progress across a sequential pipeline and to scale
/// byte progress into the active phase's slice.
///
/// Implemented for `()` (no model — phases contribute no fraction) and for
/// any `Fn(&str) -> Option<(f32, f32)>`, so a closure or a unit struct both
/// work.
pub trait PhaseModel: Send {
    fn range(&self, name: &str) -> Option<(f32, f32)>;
}

impl PhaseModel for () {
    fn range(&self, _name: &str) -> Option<(f32, f32)> {
        None
    }
}

impl<F> PhaseModel for F
where
    F: Fn(&str) -> Option<(f32, f32)> + Send,
{
    fn range(&self, name: &str) -> Option<(f32, f32)> {
        self(name)
    }
}

/// Collect a multi-line block into a single `Finding::info`. Collection starts
/// on a line equal to `trigger` (after trimming), skips an immediately
/// following `---`-style separator, and ends on the first blank line (or at
/// process exit). The joined body is emitted as `Finding::info(code, body)`.
#[derive(Debug, Clone)]
pub struct NotesConfig {
    pub trigger: String,
    pub code: String,
}

/// On-exit error synthesis. When the process exits non-zero and no
/// [`RuleAction::KnownError`] fired, [`RuleInterpreter`] emits one
/// `KnownError` so the failure carries a message instead of a bare
/// "process exited with code N".
#[derive(Debug, Clone)]
pub struct FallbackPolicy {
    /// Code attached to the synthesized `KnownError`.
    pub code: String,
    /// Buffer the first unclassified line containing "failed" (case-
    /// insensitive) and prefer it as the fallback message.
    pub from_failed_line: bool,
    /// Fall back to the last non-empty stderr line when nothing else was
    /// captured.
    pub from_stderr: bool,
}

impl Default for FallbackPolicy {
    fn default() -> Self {
        Self {
            code: "command_error".to_string(),
            from_failed_line: true,
            from_stderr: true,
        }
    }
}

impl FallbackPolicy {
    /// Set the code attached to the synthesized `KnownError`.
    pub fn code(mut self, code: impl Into<String>) -> Self {
        self.code = code.into();
        self
    }
}

/// A table-driven [`Interpreter`]. Construct with [`RuleInterpreter::new`]
/// (rules + a [`PhaseModel`]) or [`RuleInterpreter::flat`] (rules only), then
/// optionally attach [`notes`](Self::notes) and [`fallback`](Self::fallback).
///
/// Invariants it enforces, so you don't re-implement them per CLI:
///
/// - **First-match wins**, in table order.
/// - **Stream-agnostic** classification (matches on text, never the stream).
/// - **Flat sequential phases**: a new `EnterPhase` auto-exits the previous
///   one and emits boundary `Progress` so the bar never resets mid-pipeline.
/// - **No double-emit**: a rule `KnownError` suppresses the fallback.
/// - **Interpreters don't decide success** — these only enrich the Outcome.
pub struct RuleInterpreter<P: PhaseModel = ()> {
    rules: Vec<Rule>,
    phases: P,
    notes: Option<NotesConfig>,
    fallback: Option<FallbackPolicy>,

    // --- per-job state ---
    in_notes: bool,
    notes_buf: Vec<String>,
    current_phase: Option<String>,
    emitted_known_error: bool,
    fallback_error: Option<String>,
    last_stderr: Option<String>,
}

impl RuleInterpreter<()> {
    /// Rules only, no phase weighting. `EnterPhase` rules still auto-exit the
    /// previous phase; they just don't emit boundary fraction progress.
    pub fn flat(rules: Vec<Rule>) -> Self {
        Self::new(rules, ())
    }
}

impl<P: PhaseModel> RuleInterpreter<P> {
    /// Rules plus a [`PhaseModel`] for boundary/byte progress scaling.
    pub fn new(rules: Vec<Rule>, phases: P) -> Self {
        Self {
            rules,
            phases,
            notes: None,
            fallback: None,
            in_notes: false,
            notes_buf: Vec::new(),
            current_phase: None,
            emitted_known_error: false,
            fallback_error: None,
            last_stderr: None,
        }
    }

    /// Collect a multi-line block (e.g. a trailing `Notes` section) into a
    /// `Finding::info`. See [`NotesConfig`].
    pub fn notes(mut self, trigger: impl Into<String>, code: impl Into<String>) -> Self {
        self.notes = Some(NotesConfig {
            trigger: trigger.into(),
            code: code.into(),
        });
        self
    }

    /// Synthesize a `KnownError` on non-zero exit when no rule classified the
    /// failure. See [`FallbackPolicy`].
    pub fn fallback(mut self, policy: FallbackPolicy) -> Self {
        self.fallback = Some(policy);
        self
    }

    fn flush_notes(&mut self) -> Vec<InterpreterEvent> {
        self.in_notes = false;
        if self.notes_buf.is_empty() {
            return vec![];
        }
        let text = std::mem::take(&mut self.notes_buf).join("\n");
        let code = self
            .notes
            .as_ref()
            .map(|n| n.code.clone())
            .unwrap_or_else(|| "notes".to_string());
        vec![InterpreterEvent::Finding {
            finding: Finding::info(code, text),
        }]
    }
}

impl<P: PhaseModel> Interpreter for RuleInterpreter<P> {
    fn on_line(&mut self, _ctx: &Context, line: &Line) -> Vec<InterpreterEvent> {
        let text = &line.text;

        // --- Multi-line notes block (terminates on a blank line) ---
        if let Some(notes) = self.notes.as_ref() {
            let trigger = notes.trigger.clone();
            if self.in_notes {
                if text.trim_start().starts_with("---") {
                    return vec![];
                }
                if text.trim().is_empty() {
                    return self.flush_notes();
                }
                self.notes_buf.push(text.clone());
                return vec![];
            }
            if text.trim() == trigger {
                self.in_notes = true;
                return vec![];
            }
        }

        if text.is_empty() {
            return vec![];
        }

        if line.stream == Stream::Stderr {
            self.last_stderr = Some(text.clone());
        }

        for rule in self.rules.iter() {
            let Some(caps) = rule.regex.captures(text) else {
                continue;
            };
            return match &rule.action {
                RuleAction::Benign => vec![],
                RuleAction::Progress(label) => vec![InterpreterEvent::Progress {
                    progress: Progress::indeterminate(label.clone()),
                }],
                RuleAction::EnterPhase {
                    name,
                    label_template,
                } => {
                    let label = render(label_template, &caps);
                    let mut events = Vec::with_capacity(4);
                    if let Some(prev) = self.current_phase.take() {
                        events.push(InterpreterEvent::ExitPhase);
                        if let Some((_, end)) = self.phases.range(&prev) {
                            events.push(InterpreterEvent::Progress {
                                progress: Progress::fraction(end),
                            });
                        }
                    }
                    self.current_phase = Some(name.clone());
                    events.push(InterpreterEvent::EnterPhase {
                        name: name.clone(),
                        label: Some(label),
                    });
                    if let Some((start, _)) = self.phases.range(name) {
                        events.push(InterpreterEvent::Progress {
                            progress: Progress::fraction(start),
                        });
                    }
                    events
                }
                RuleAction::Warning { code, template } => vec![InterpreterEvent::Warning {
                    code: Some(code.clone()),
                    message: render(template, &caps),
                }],
                RuleAction::KnownError { code, template } => {
                    let message = render(template, &caps);
                    self.emitted_known_error = true;
                    self.fallback_error = None;
                    vec![InterpreterEvent::KnownError {
                        code: code.clone(),
                        message,
                    }]
                }
                RuleAction::Summary { template } => vec![InterpreterEvent::Summary {
                    text: render(template, &caps),
                }],
                RuleAction::ByteProgressMb => {
                    let done = caps.get(1).and_then(|m| m.as_str().parse::<f64>().ok());
                    let total = caps.get(2).and_then(|m| m.as_str().parse::<f64>().ok());
                    match (done, total) {
                        (Some(d), Some(t)) if t > 0.0 => {
                            let ratio = (d / t).clamp(0.0, 1.0) as f32;
                            let fraction = self
                                .current_phase
                                .as_deref()
                                .and_then(|p| self.phases.range(p))
                                .map(|(start, end)| start + ratio * (end - start))
                                .unwrap_or(ratio);
                            vec![InterpreterEvent::Progress {
                                progress: Progress::fraction(fraction),
                            }]
                        }
                        _ => vec![],
                    }
                }
            };
        }

        // Unclassified — soft-buffer "failed"-looking text for the on-exit
        // fallback. First mention wins.
        if let Some(fb) = self.fallback.as_ref() {
            if fb.from_failed_line
                && self.fallback_error.is_none()
                && text.to_ascii_lowercase().contains("failed")
            {
                self.fallback_error = Some(text.clone());
            }
        }

        vec![]
    }

    fn on_exit(&mut self, _ctx: &Context, exit: &ExitCode) -> Vec<InterpreterEvent> {
        // Flush an unterminated notes block (process ended mid-block).
        let mut events = self.flush_notes();

        // Close any dangling phase and advance the bar to its end so the
        // final visual reads as "complete" rather than wherever progress
        // happened to pause.
        if let Some(prev) = self.current_phase.take() {
            events.push(InterpreterEvent::ExitPhase);
            if let Some((_, end)) = self.phases.range(&prev) {
                events.push(InterpreterEvent::Progress {
                    progress: Progress::fraction(end),
                });
            }
        }

        let Some(fb) = self.fallback.as_ref() else {
            return events;
        };
        if exit.is_success() || self.emitted_known_error {
            return events;
        }
        let mut message = None;
        if fb.from_failed_line {
            message = self.fallback_error.take();
        }
        if message.is_none() && fb.from_stderr {
            message = self.last_stderr.take();
        }
        if let Some(message) = message {
            events.push(InterpreterEvent::KnownError {
                code: fb.code.clone(),
                message,
            });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::interpreter::{Context, Line};
    use crate::JobId;

    fn drive(
        interp: &mut dyn Interpreter,
        lines: &[(Stream, &str)],
        exit: ExitCode,
    ) -> Vec<InterpreterEvent> {
        let now = std::time::SystemTime::now();
        let cmd = Command::new("dummy");
        let spec = cmd.spec();
        let ctx = Context {
            job: JobId::new(),
            command: spec,
            current_phase: None,
            phase_stack: &[],
            elapsed: std::time::Duration::ZERO,
        };
        let mut out = Vec::new();
        for (stream, text) in lines {
            out.extend(interp.on_line(
                &ctx,
                &Line {
                    stream: *stream,
                    text: (*text).to_string(),
                    at: now,
                },
            ));
        }
        out.extend(interp.on_exit(&ctx, &exit));
        out
    }

    struct P;
    impl PhaseModel for P {
        fn range(&self, name: &str) -> Option<(f32, f32)> {
            match name {
                "download" => Some((0.05, 0.25)),
                "extract" => Some((0.30, 0.50)),
                _ => None,
            }
        }
    }

    #[test]
    fn first_match_wins_and_benign_is_silent() {
        let mut i = RuleInterpreter::flat(rules![
            benign, r"^done\.?$";
            known, "x.err", "$1", r"^(?i)ERROR:\s*(.+)$";
        ]);
        assert!(drive(&mut i, &[(Stream::Stdout, "done.")], ExitCode::from_code(0)).is_empty());
        let evs = drive(
            &mut RuleInterpreter::flat(rules![known, "x.err", "$1", r"^(?i)ERROR:\s*(.+)$"]),
            &[(Stream::Stderr, "ERROR: boom")],
            ExitCode::from_code(1),
        );
        assert!(matches!(
            evs.as_slice(),
            [InterpreterEvent::KnownError { code, message }]
                if code == "x.err" && message == "boom"
        ));
    }

    #[test]
    fn flat_phases_auto_exit_and_emit_boundary_progress() {
        let mut i = RuleInterpreter::new(
            rules![
                enter_phase, "download", "DL $1", r"^Downloading '([^']+)'";
                enter_phase, "extract", "Extract", r"^Extracting\b";
            ],
            P,
        );
        let evs = drive(
            &mut i,
            &[
                (Stream::Stdout, "Downloading 'a.zip'"),
                (Stream::Stdout, "Extracting a.zip"),
            ],
            ExitCode::from_code(0),
        );
        let kinds: Vec<&str> = evs
            .iter()
            .filter_map(|e| match e {
                InterpreterEvent::EnterPhase { .. } => Some("enter"),
                InterpreterEvent::ExitPhase => Some("exit"),
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec!["enter", "exit", "enter", "exit"]);
        let last = evs
            .iter()
            .filter_map(|e| match e {
                InterpreterEvent::Progress { progress } => progress.as_fraction(),
                _ => None,
            })
            .last()
            .unwrap();
        assert!((last - 0.50).abs() < 1e-4, "final fraction {last}");
    }

    #[test]
    fn byte_progress_scales_into_phase() {
        let mut i = RuleInterpreter::new(
            rules![
                enter_phase, "download", "DL", r"^Downloading\b";
                byte_progress_mb, r"([\d.]+) MB / ([\d.]+) MB";
            ],
            P,
        );
        let evs = drive(
            &mut i,
            &[
                (Stream::Stdout, "Downloading"),
                (Stream::Stdout, "12.34 MB / 50.00 MB"),
            ],
            ExitCode::from_code(0),
        );
        let fracs: Vec<f32> = evs
            .iter()
            .filter_map(|e| match e {
                InterpreterEvent::Progress { progress } => progress.as_fraction(),
                _ => None,
            })
            .collect();
        // 0.2468 ratio into [0.05, 0.25] ≈ 0.099
        assert!(fracs.iter().any(|f| (f - 0.099).abs() < 0.01), "{fracs:?}");
    }

    #[test]
    fn notes_block_becomes_finding() {
        let mut i = RuleInterpreter::flat(vec![]).notes("Notes", "x.notes");
        let evs = drive(
            &mut i,
            &[
                (Stream::Stdout, "Notes"),
                (Stream::Stdout, "-----"),
                (Stream::Stdout, "line one"),
                (Stream::Stdout, "line two"),
                (Stream::Stdout, ""),
            ],
            ExitCode::from_code(0),
        );
        assert!(evs.iter().any(|e| matches!(e,
            InterpreterEvent::Finding { finding }
                if finding.code == "x.notes"
                && finding.message.contains("line one")
                && finding.message.contains("line two"))));
    }

    #[test]
    fn fallback_synthesizes_known_error_only_when_unclassified() {
        // Unclassified failure → fallback fires.
        let evs = drive(
            &mut RuleInterpreter::flat(vec![]).fallback(FallbackPolicy::default().code("x.cmd")),
            &[(Stream::Stderr, "weird diagnostic")],
            ExitCode::from_code(7),
        );
        assert!(matches!(
            evs.last(),
            Some(InterpreterEvent::KnownError { code, .. }) if code == "x.cmd"
        ));

        // Rule already classified → no double-emit.
        let evs = drive(
            &mut RuleInterpreter::flat(rules![known, "x.err", "$1", r"^(?i)ERROR:\s*(.+)$"])
                .fallback(FallbackPolicy::default()),
            &[(Stream::Stderr, "ERROR: boom")],
            ExitCode::from_code(1),
        );
        assert_eq!(
            evs.iter()
                .filter(|e| matches!(e, InterpreterEvent::KnownError { .. }))
                .count(),
            1
        );

        // Clean exit → never fires.
        let evs = drive(
            &mut RuleInterpreter::flat(vec![]).fallback(FallbackPolicy::default()),
            &[(Stream::Stderr, "harmless chatter")],
            ExitCode::from_code(0),
        );
        assert!(!evs
            .iter()
            .any(|e| matches!(e, InterpreterEvent::KnownError { .. })));
    }
}
