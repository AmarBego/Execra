use std::path::PathBuf;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Recommendation,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    Command {
        label: String,
        program: String,
        args: Vec<String>,
        cwd: Option<PathBuf>,
    },
    Link {
        label: String,
        url: String,
    },
    Instruction {
        label: String,
        text: String,
    },
}

impl Action {
    pub fn command<I, S>(label: impl Into<String>, program: impl Into<String>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Action::Command {
            label: label.into(),
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
        }
    }

    pub fn command_in<I, S>(
        label: impl Into<String>,
        program: impl Into<String>,
        args: I,
        cwd: impl Into<PathBuf>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Action::Command {
            label: label.into(),
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: Some(cwd.into()),
        }
    }

    pub fn link(label: impl Into<String>, url: impl Into<String>) -> Self {
        Action::Link {
            label: label.into(),
            url: url.into(),
        }
    }

    pub fn instruction(label: impl Into<String>, text: impl Into<String>) -> Self {
        Action::Instruction {
            label: label.into(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum RelatedEntity {
    Package(String),
    File(PathBuf),
    Url(String),
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub action: Option<Action>,
    pub related: Option<RelatedEntity>,
    pub at: SystemTime,
}

impl Finding {
    fn make(severity: Severity, code: impl Into<String>, message: impl Into<String>) -> Self {
        Finding {
            severity,
            code: code.into(),
            message: message.into(),
            action: None,
            related: None,
            at: SystemTime::now(),
        }
    }

    pub fn info(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::make(Severity::Info, code, message)
    }

    pub fn recommendation(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::make(Severity::Recommendation, code, message)
    }

    pub fn warning(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::make(Severity::Warning, code, message)
    }

    pub fn error(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::make(Severity::Error, code, message)
    }

    pub fn with_action(mut self, action: Action) -> Self {
        self.action = Some(action);
        self
    }

    pub fn with_related(mut self, related: RelatedEntity) -> Self {
        self.related = Some(related);
        self
    }
}
