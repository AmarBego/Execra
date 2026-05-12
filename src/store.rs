//! SQLite persistence. Stores jobs (one row), events (JSON blob per row), and
//! findings (denormalized for queryability). Raw `OutputAppended` events are
//! intentionally *not* persisted to SQLite , they belong in flat files
//! (next pass). High-volume CLIs would balloon the database otherwise.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::event::Event;
use crate::job::{Job, JobId, JobState};
use crate::outcome::Outcome;

pub const SCHEMA_VERSION: i64 = 0;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported schema version {found}; maximum supported is {supported}")]
    UnsupportedSchemaVersion { found: i64, supported: i64 },
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone, Default)]
pub struct JobsFilter {
    pub tag: Option<String>,
    pub state: Option<JobState>,
    pub created_after: Option<SystemTime>,
    pub created_before: Option<SystemTime>,
}

impl Store {
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// In-memory store, useful for tests.
    pub async fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Store {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn upsert_job(&self, job: &Job) -> Result<(), StoreError> {
        let conn = self.conn.lock().await;
        let id = job.id.0.as_bytes().to_vec();
        let command_json = serde_json::to_string(&job.command)?;
        let outcome_json = match &job.outcome {
            Some(o) => Some(serde_json::to_string(o)?),
            None => None,
        };
        let state = serde_json::to_string(&job.state)?
            .trim_matches('"')
            .to_string();
        conn.execute(
            "INSERT INTO jobs (\
                id, command_json, label, tags_json, created_at_ms, started_at_ms, \
                state, exit_code, exit_signal, outcome_json, schema_version\
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT(id) DO UPDATE SET \
                label = excluded.label, \
                started_at_ms = excluded.started_at_ms, \
                state = excluded.state, \
                exit_code = excluded.exit_code, \
                exit_signal = excluded.exit_signal, \
                outcome_json = excluded.outcome_json",
            params![
                id,
                command_json,
                job.label,
                serde_json::to_string(&job.command.tags)?,
                to_ms(job.created_at),
                job.started_at.map(to_ms),
                state,
                job.exit.and_then(|e| e.code),
                job.exit.and_then(|e| e.signal),
                outcome_json,
                SCHEMA_VERSION,
            ],
        )?;
        Ok(())
    }

    pub async fn insert_event(&self, event: &Event) -> Result<(), StoreError> {
        // OutputAppended is high-volume and belongs in flat files.
        if matches!(event, Event::OutputAppended { .. }) {
            return Ok(());
        }
        let conn = self.conn.lock().await;
        let job_id = event.job_id().0.as_bytes().to_vec();
        let kind = event_kind(event);
        let payload = serde_json::to_string(event)?;
        conn.execute(
            "INSERT INTO events (job_id, at_ms, kind, payload, schema_version) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![job_id, to_ms(event.at()), kind, payload, SCHEMA_VERSION],
        )?;
        // Findings get denormalized for cheap queries (e.g. "all errors").
        if let Event::FindingEmitted { finding, .. } = event {
            let finding_json = serde_json::to_string(finding)?;
            let severity = serde_json::to_string(&finding.severity)?
                .trim_matches('"')
                .to_string();
            conn.execute(
                "INSERT INTO findings (job_id, at_ms, code, severity, finding_json) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    job_id,
                    to_ms(finding.at),
                    finding.code,
                    severity,
                    finding_json,
                ],
            )?;
        }
        Ok(())
    }

    pub async fn load_job(&self, id: JobId) -> Result<Option<Job>, StoreError> {
        let conn = self.conn.lock().await;
        let id_bytes = id.0.as_bytes().to_vec();
        let mut stmt = conn.prepare(
            "SELECT command_json, label, created_at_ms, started_at_ms, state, \
                    exit_code, exit_signal, outcome_json, schema_version \
             FROM jobs WHERE id = ?1",
        )?;
        let row = stmt.query_row(params![id_bytes], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, i64>(8)?,
            ))
        });
        match row {
            Ok((
                cmd_json,
                label,
                created_ms,
                started_ms,
                state,
                code,
                signal,
                oc_json,
                schema_version,
            )) => {
                check_schema_version(schema_version)?;
                let job = build_job(
                    id, cmd_json, label, created_ms, started_ms, state, code, signal, oc_json,
                )?;
                Ok(Some(job))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Most recent jobs first. Used by `Execra::jobs()` resolution and by the
    /// CLI's `ls` command.
    pub async fn list_jobs(
        &self,
        limit: usize,
        filter: &JobsFilter,
    ) -> Result<Vec<Job>, StoreError> {
        let conn = self.conn.lock().await;
        let mut sql = String::from(
            "SELECT id, command_json, label, created_at_ms, started_at_ms, state, \
                    exit_code, exit_signal, outcome_json, schema_version FROM jobs",
        );
        let mut clauses = Vec::new();
        let mut values = Vec::new();
        if let Some(state) = filter.state {
            clauses.push("state = ?".to_string());
            values.push(Value::Text(state_to_string(state)?));
        }
        if let Some(after) = filter.created_after {
            clauses.push("created_at_ms >= ?".to_string());
            values.push(Value::Integer(to_ms(after)));
        }
        if let Some(before) = filter.created_before {
            clauses.push("created_at_ms <= ?".to_string());
            values.push(Value::Integer(to_ms(before)));
        }
        if let Some(tag) = &filter.tag {
            clauses.push("tags_json LIKE ?".to_string());
            values.push(Value::Text(format!("%\"{}\"%", tag.replace('"', "\\\""))));
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at_ms DESC LIMIT ?");
        values.push(Value::Integer(limit as i64));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (
                id_bytes,
                cmd_json,
                label,
                created_ms,
                started_ms,
                state,
                code,
                signal,
                oc_json,
                schema_version,
            ) = r?;
            check_schema_version(schema_version)?;
            let id = JobId(uuid::Uuid::from_slice(&id_bytes).map_err(|e| {
                StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Blob,
                    Box::new(e),
                ))
            })?);
            out.push(build_job(
                id, cmd_json, label, created_ms, started_ms, state, code, signal, oc_json,
            )?);
        }
        Ok(out)
    }

    pub async fn list_events(&self, job: JobId, limit: usize) -> Result<Vec<Event>, StoreError> {
        let conn = self.conn.lock().await;
        let job_id = job.0.as_bytes().to_vec();
        let mut stmt = conn.prepare(
            "SELECT payload, schema_version FROM events WHERE job_id = ?1 ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![job_id, limit as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (payload, schema_version) = row?;
            check_schema_version(schema_version)?;
            out.push(serde_json::from_str(&payload)?);
        }
        Ok(out)
    }

    /// Mark every Running/Exited job from a previous process as Failed with
    /// `SpawnFailed { error: "host process exited" }`. Called from
    /// `Execra::open` per RUNTIME.md's resumability rule.
    pub async fn resurrect_stranded_jobs(&self) -> Result<usize, StoreError> {
        let conn = self.conn.lock().await;
        let outcome = Outcome::Failed {
            reason: crate::outcome::FailureReason::SpawnFailed {
                error: "host process exited".into(),
            },
            summary: None,
            findings: vec![],
        };
        let outcome_json = serde_json::to_string(&outcome)?;
        let n = conn.execute(
            "UPDATE jobs SET state = 'finalized', outcome_json = ?1 \
             WHERE state IN ('queued', 'running', 'exited')",
            params![outcome_json],
        )?;
        Ok(n)
    }
}

#[allow(clippy::too_many_arguments)]
fn build_job(
    id: JobId,
    command_json: String,
    label: Option<String>,
    created_ms: i64,
    started_ms: Option<i64>,
    state_str: String,
    exit_code: Option<i64>,
    exit_signal: Option<i64>,
    outcome_json: Option<String>,
) -> Result<Job, StoreError> {
    let command = serde_json::from_str(&command_json)?;
    let state: JobState = serde_json::from_str(&format!("\"{state_str}\""))?;
    let outcome = match outcome_json {
        Some(j) => Some(serde_json::from_str(&j)?),
        None => None,
    };
    let exit = if exit_code.is_some() || exit_signal.is_some() {
        Some(crate::outcome::ExitCode {
            code: exit_code.map(|c| c as i32),
            signal: exit_signal.map(|s| s as i32),
        })
    } else {
        None
    };
    Ok(Job {
        id,
        command,
        created_at: from_ms(created_ms),
        started_at: started_ms.map(from_ms),
        state,
        current_phase: None,
        progress: crate::progress::Progress::Unknown,
        label,
        exit,
        outcome,
    })
}

fn event_kind(event: &Event) -> &'static str {
    match event {
        Event::JobCreated { .. } => "job_created",
        Event::JobStarted { .. } => "job_started",
        Event::PhaseEntered { .. } => "phase_entered",
        Event::PhaseUpdated { .. } => "phase_updated",
        Event::PhaseExited { .. } => "phase_exited",
        Event::ProgressUpdated { .. } => "progress_updated",
        Event::LabelUpdated { .. } => "label_updated",
        Event::OutputAppended { .. } => "output_appended",
        Event::WarningDetected { .. } => "warning_detected",
        Event::KnownErrorDetected { .. } => "known_error_detected",
        Event::FindingEmitted { .. } => "finding_emitted",
        Event::PromptDetected { .. } => "prompt_detected",
        Event::InterpreterError { .. } => "interpreter_error",
        Event::Exited { .. } => "exited",
        Event::Finalized { .. } => "finalized",
        Event::Cancelled { .. } => "cancelled",
    }
}

fn check_schema_version(found: i64) -> Result<(), StoreError> {
    if found > SCHEMA_VERSION {
        return Err(StoreError::UnsupportedSchemaVersion {
            found,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

fn state_to_string(state: JobState) -> Result<String, StoreError> {
    Ok(serde_json::to_string(&state)?.trim_matches('"').to_string())
}

fn to_ms(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn from_ms(ms: i64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_millis(ms.max(0) as u64)
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS jobs (
    id              BLOB PRIMARY KEY,
    command_json    TEXT NOT NULL,
    label           TEXT,
    tags_json       TEXT NOT NULL DEFAULT '[]',
    created_at_ms   INTEGER NOT NULL,
    started_at_ms   INTEGER,
    state           TEXT NOT NULL,
    exit_code       INTEGER,
    exit_signal     INTEGER,
    outcome_json    TEXT,
    schema_version  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS jobs_created_idx ON jobs(created_at_ms DESC);
CREATE INDEX IF NOT EXISTS jobs_state_idx   ON jobs(state);

CREATE TABLE IF NOT EXISTS events (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id          BLOB NOT NULL,
    at_ms           INTEGER NOT NULL,
    kind            TEXT NOT NULL,
    payload         TEXT NOT NULL,
    schema_version  INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS events_job_idx ON events(job_id, id);

CREATE TABLE IF NOT EXISTS findings (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id          BLOB NOT NULL,
    at_ms           INTEGER NOT NULL,
    code            TEXT NOT NULL,
    severity        TEXT NOT NULL,
    finding_json    TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS findings_job_idx      ON findings(job_id);
CREATE INDEX IF NOT EXISTS findings_severity_idx ON findings(severity);
"#;
