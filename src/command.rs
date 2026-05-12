use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::interpreter::Interpreter;

/// Stdin policy. Execra does not model interactive stdin; feed bytes or inherit.
pub enum StdinMode {
    Null,
    Inherit,
    Piped(Vec<u8>),
}

impl Default for StdinMode {
    fn default() -> Self {
        StdinMode::Null
    }
}

impl std::fmt::Debug for StdinMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StdinMode::Null => f.write_str("Null"),
            StdinMode::Inherit => f.write_str("Inherit"),
            StdinMode::Piped(bytes) => write!(f, "Piped({} bytes)", bytes.len()),
        }
    }
}

/// Clone/serialize-safe view of a command. This is what gets persisted into
/// `Job.command` and surfaced in `Event::JobCreated`.
///
/// Fields that the OS expresses as `OsString` are stored here as plain
/// `String`. On Windows, paths that aren't valid UTF-16 (extremely rare in
/// practice) are converted lossily at the spawn boundary — the wire format
/// stays clean and the DB stays human-readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub env_clear: bool,
    pub cwd: Option<PathBuf>,
    pub label: Option<String>,
    pub tags: Vec<String>,
    pub timeout: Option<Duration>,
    pub hide_window: bool,
}

/// Builder + interpreter slot. Pass to `Execra::spawn`.
///
/// Not `Clone` because the interpreter is a heap-stored trait object; if
/// you need a cloneable view of the command, use [`Command::spec`].
pub struct Command {
    spec: CommandSpec,
    stdin: StdinMode,
    interpreter: Option<Box<dyn Interpreter + Send + 'static>>,
}

impl Command {
    pub fn new(program: impl Into<String>) -> Self {
        Command {
            spec: CommandSpec {
                program: program.into(),
                args: Vec::new(),
                env: Vec::new(),
                env_clear: false,
                cwd: None,
                label: None,
                tags: Vec::new(),
                timeout: None,
                hide_window: default_hide_window(),
            },
            stdin: StdinMode::Null,
            interpreter: None,
        }
    }

    /// Runs a script through the platform's default non-interactive shell:
    /// `cmd /C` on Windows and `sh -c` elsewhere.
    pub fn shell(script: impl Into<String>) -> Self {
        Self::system_shell(script)
    }

    /// Runs a script through the platform's default non-interactive shell:
    /// `cmd /C` on Windows and `sh -c` elsewhere.
    pub fn system_shell(script: impl Into<String>) -> Self {
        let script = script.into();
        #[cfg(windows)]
        {
            Command::new("cmd").args(["/C", &script])
        }
        #[cfg(not(windows))]
        {
            Command::new("sh").args(["-c", &script])
        }
    }

    /// Runs a script through `cmd /C`.
    pub fn cmd(script: impl Into<String>) -> Self {
        let script = script.into();
        Command::new("cmd").args(["/C", &script])
    }

    /// Runs a script through `sh -c`.
    pub fn sh(script: impl Into<String>) -> Self {
        let script = script.into();
        Command::new("sh").args(["-c", &script])
    }

    /// Runs a script through Windows PowerShell.
    pub fn powershell(script: impl Into<String>) -> Self {
        let script = script.into();
        Command::new("powershell").args(["-NoProfile", "-Command", &script])
    }

    /// Runs a script through PowerShell 7+ (`pwsh`).
    pub fn pwsh(script: impl Into<String>) -> Self {
        let script = script.into();
        Command::new("pwsh").args(["-NoProfile", "-Command", &script])
    }

    pub fn arg(mut self, a: impl Into<String>) -> Self {
        self.spec.args.push(a.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.spec.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.spec.env.push((key.into(), val.into()));
        self
    }

    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        self.spec
            .env
            .extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
        self
    }

    pub fn env_clear(mut self) -> Self {
        self.spec.env_clear = true;
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.spec.cwd = Some(dir.into());
        self
    }

    pub fn stdin(mut self, mode: StdinMode) -> Self {
        self.stdin = mode;
        self
    }

    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }

    pub fn tags(mut self, tags: impl IntoIterator<Item = String>) -> Self {
        self.spec.tags.extend(tags);
        self
    }

    pub fn interpreter<I: Interpreter + Send + 'static>(mut self, i: I) -> Self {
        self.interpreter = Some(Box::new(i));
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.spec.timeout = Some(d);
        self
    }

    pub fn hide_window(mut self, yes: bool) -> Self {
        self.spec.hide_window = yes;
        self
    }

    pub fn spec(&self) -> &CommandSpec {
        &self.spec
    }

    /// Consumes the command, returning the cloneable spec, the stdin mode,
    /// and the interpreter slot. Used by the runtime at spawn time.
    pub fn into_parts(
        self,
    ) -> (
        CommandSpec,
        StdinMode,
        Option<Box<dyn Interpreter + Send + 'static>>,
    ) {
        (self.spec, self.stdin, self.interpreter)
    }
}

#[cfg(windows)]
fn default_hide_window() -> bool {
    true
}

#[cfg(not(windows))]
fn default_hide_window() -> bool {
    false
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("spec", &self.spec)
            .field("stdin", &self.stdin)
            .field("interpreter", &self.interpreter.is_some())
            .finish()
    }
}
