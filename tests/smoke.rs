//! End-to-end smoke tests for the runtime. Uses platform shell so it can
//! run on both Windows and Unix without bundling test fixtures.

#[cfg(feature = "gzip")]
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use execra::{
    Command, Config, Context, Execra, ExitCode, Finding, Interpreter, InterpreterEvent as Ev, Job,
    JobId, JobState, Line, Outcome, Progress, RawOutputPolicy,
};

static TEST_ID: AtomicUsize = AtomicUsize::new(0);

fn test_config() -> Config {
    let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("execra-test-{}-{id}", std::process::id()));
    Config {
        db_path: root.join("execra.db"),
        log_dir: root.join("logs"),
        ..Config::default()
    }
}

fn echo(text: &str) -> Command {
    Command::shell(format!("echo {text}"))
}

fn fail() -> Command {
    Command::shell("exit 7")
}

fn sleep_forever() -> Command {
    #[cfg(windows)]
    {
        // `ping -t` runs until killed; -n 9999 also works and is quieter.
        Command::new("cmd").args(["/C", "ping -n 9999 127.0.0.1 > nul"])
    }
    #[cfg(not(windows))]
    {
        Command::new("sh").args(["-c", "sleep 60"])
    }
}

fn sleep_millis(ms: u64) -> Command {
    #[cfg(windows)]
    {
        Command::new("powershell").args([
            "-NoProfile",
            "-Command",
            &format!("Start-Sleep -Milliseconds {ms}"),
        ])
    }
    #[cfg(not(windows))]
    {
        Command::new("sh").args(["-c", &format!("sleep {}", ms as f64 / 1000.0)])
    }
}

fn invalid_utf8_output() -> Command {
    #[cfg(windows)]
    {
        Command::powershell("[Console]::OpenStandardOutput().Write([byte[]](255,10),0,2)")
    }
    #[cfg(not(windows))]
    {
        Command::sh("printf '\\377\\n'")
    }
}

#[tokio::test]
async fn successful_command_succeeds() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt.spawn(echo("hello")).await.unwrap();
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
}

#[tokio::test]
async fn invalid_utf8_output_is_lossy_decoded_not_dropped() {
    let rt = Execra::open(test_config()).await.unwrap();
    let mut handle = rt.spawn(invalid_utf8_output()).await.unwrap();
    let mut events = handle.subscribe();
    let mut saw_replacement = false;
    while let Some(event) = events.next().await {
        match event {
            execra::Event::OutputAppended { line, .. } if line.contains('\u{fffd}') => {
                saw_replacement = true;
            }
            execra::Event::Finalized { .. } => break,
            _ => {}
        }
    }
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
    assert!(saw_replacement, "invalid output was not surfaced lossily");
}

#[tokio::test]
async fn non_zero_exit_fails_with_nonzero_exit_reason() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt.spawn(fail()).await.unwrap();
    let outcome = handle.await;
    match outcome {
        Outcome::Failed { reason, .. } => match reason {
            execra::FailureReason::NonZeroExit { code } => assert_eq!(code, 7),
            other => panic!("expected NonZeroExit, got {other:?}"),
        },
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[tokio::test]
async fn spawn_failure_yields_spawn_failed_outcome() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt
        .spawn(Command::new("definitely-not-a-real-binary-9f8e2"))
        .await
        .unwrap();
    let outcome = handle.await;
    assert!(matches!(
        outcome,
        Outcome::Failed {
            reason: execra::FailureReason::SpawnFailed { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn cancelled_job_finalizes_as_cancelled() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt.spawn(sleep_forever()).await.unwrap();
    // Give the child a moment to actually start.
    tokio::time::sleep(Duration::from_millis(100)).await;
    handle.cancel().unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("cancelled job should finalize quickly");
    assert!(
        matches!(outcome, Outcome::Cancelled { .. }),
        "got {outcome:?}"
    );
}

#[tokio::test]
async fn cancelling_finalized_job_is_ok() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt.spawn(echo("done")).await.unwrap();
    let id = handle.id();
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
    rt.cancel(id).unwrap();
}

#[tokio::test]
async fn max_concurrent_serializes_process_execution() {
    let mut config = test_config();
    config.max_concurrent = 1;
    let rt = Execra::open(config).await.unwrap();
    let start = std::time::Instant::now();
    let first = rt.spawn(sleep_millis(350)).await.unwrap();
    let second = rt.spawn(sleep_millis(350)).await.unwrap();
    let first = first.await;
    let second = second.await;
    assert!(matches!(first, Outcome::Succeeded { .. }), "got {first:?}");
    assert!(
        matches!(second, Outcome::Succeeded { .. }),
        "got {second:?}"
    );
    assert!(
        start.elapsed() >= Duration::from_millis(600),
        "jobs appear to have run concurrently"
    );
}

#[tokio::test]
async fn timed_out_job_fails_with_timeout_reason() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt
        .spawn(sleep_forever().timeout(Duration::from_millis(100)))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("timed-out job should finalize quickly");
    assert!(
        matches!(
            outcome,
            Outcome::Failed {
                reason: execra::FailureReason::Timeout,
                ..
            }
        ),
        "got {outcome:?}"
    );
}

// ---------- Interpreter-driven flows ----------

struct CollectFinding;
impl Interpreter for CollectFinding {
    fn on_line(&mut self, _ctx: &Context, line: &Line) -> Vec<Ev> {
        if line.text.contains("ALERT") {
            return vec![Ev::Finding {
                finding: Finding::warning("test.alert", line.text.clone()),
            }];
        }
        if line.text.contains("HALF") {
            return vec![Ev::Progress {
                progress: Progress::fraction(0.5),
            }];
        }
        vec![]
    }
    fn on_exit(&mut self, _ctx: &Context, _exit: &ExitCode) -> Vec<Ev> {
        vec![Ev::Summary {
            text: "done".into(),
        }]
    }
}

#[tokio::test]
async fn interpreter_findings_persist_into_outcome() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt
        .spawn(echo("ALERT something happened").interpreter(CollectFinding))
        .await
        .unwrap();
    let outcome = handle.await;
    match outcome {
        Outcome::Succeeded { summary, findings } => {
            assert_eq!(summary.as_deref(), Some("done"));
            assert_eq!(findings.len(), 1);
            assert_eq!(findings[0].code, "test.alert");
        }
        other => panic!("expected Succeeded, got {other:?}"),
    }
}

struct PanicOnce {
    fired: bool,
}
impl Interpreter for PanicOnce {
    fn on_line(&mut self, _ctx: &Context, _line: &Line) -> Vec<Ev> {
        if !self.fired {
            self.fired = true;
            panic!("interpreter blew up on first line");
        }
        vec![]
    }
    fn on_exit(&mut self, _ctx: &Context, _exit: &ExitCode) -> Vec<Ev> {
        vec![]
    }
}

#[tokio::test]
async fn interpreter_panic_does_not_kill_job() {
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt
        .spawn(echo("first line").interpreter(PanicOnce { fired: false }))
        .await
        .unwrap();
    let outcome = handle.await;
    // The process still exits 0 even though the interpreter panicked.
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
}

#[tokio::test]
async fn known_error_makes_failed_carry_known_error_reason() {
    struct ClaimKnownError;
    impl Interpreter for ClaimKnownError {
        fn on_line(&mut self, _: &Context, _: &Line) -> Vec<Ev> {
            vec![]
        }
        fn on_exit(&mut self, _: &Context, exit: &ExitCode) -> Vec<Ev> {
            // Classify the failure at exit time based on the captured code.
            // Mirrors the real-world pattern where an interpreter recognizes
            // a known failure mode after the fact.
            if !exit.is_success() {
                vec![Ev::KnownError {
                    code: "test.broken".into(),
                    message: "explicitly broken".into(),
                }]
            } else {
                vec![]
            }
        }
    }
    let rt = Execra::open(test_config()).await.unwrap();
    let handle = rt.spawn(fail().interpreter(ClaimKnownError)).await.unwrap();
    let outcome = handle.await;
    match outcome {
        Outcome::Failed {
            reason: execra::FailureReason::KnownError { code, .. },
            ..
        } => assert_eq!(code, "test.broken"),
        other => panic!("expected Failed{{KnownError}}, got {other:?}"),
    }
}

#[tokio::test]
async fn completed_job_survives_reopen_and_jobs_query_runs() {
    let config = test_config();
    let rt = Execra::open(config.clone()).await.unwrap();
    let handle = rt
        .spawn(
            echo("persist me")
                .label("persistent")
                .tags(vec!["persist".to_string()]),
        )
        .await
        .unwrap();
    let id = handle.id();
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );

    let reopened = Execra::open(config).await.unwrap();
    let job = reopened.job(id).await.expect("persisted job should load");
    assert_eq!(job.state, JobState::Finalized);
    assert_eq!(job.label.as_deref(), Some("persistent"));

    let jobs = reopened
        .jobs()
        .with_tag("persist")
        .run(&reopened)
        .await
        .unwrap();
    assert!(
        jobs.iter().any(|j| j.id == id),
        "persisted job missing from query"
    );
}

#[tokio::test]
async fn jobs_query_filters_by_tag_state_and_created_time() {
    let config = test_config();
    let rt = Execra::open(config).await.unwrap();
    let before = SystemTime::now() - Duration::from_secs(1);
    let keep = rt
        .spawn(echo("keep").tags(vec!["keep".to_string()]))
        .await
        .unwrap();
    let keep_id = keep.id();
    let skip = rt
        .spawn(echo("skip").tags(vec!["skip".to_string()]))
        .await
        .unwrap();
    let skip_id = skip.id();
    assert!(matches!(keep.await, Outcome::Succeeded { .. }));
    assert!(matches!(skip.await, Outcome::Succeeded { .. }));
    let after = SystemTime::now() + Duration::from_secs(1);

    let jobs = rt
        .jobs()
        .with_tag("keep")
        .with_state(JobState::Finalized)
        .created_after(before)
        .created_before(after)
        .limit(10)
        .run(&rt)
        .await
        .unwrap();
    assert!(jobs.iter().any(|j| j.id == keep_id));
    assert!(!jobs.iter().any(|j| j.id == skip_id));
}

#[tokio::test]
async fn open_marks_stranded_running_jobs_failed() {
    let config = test_config();
    let store = execra::store::Store::open(&config.db_path).await.unwrap();
    let id = JobId::new();
    let job = Job {
        id,
        command: Command::new("stranded").spec().clone(),
        created_at: std::time::SystemTime::now(),
        started_at: Some(std::time::SystemTime::now()),
        state: JobState::Running,
        current_phase: None,
        progress: Progress::Unknown,
        label: Some("stranded".into()),
        exit: None,
        outcome: None,
    };
    store.upsert_job(&job).await.unwrap();
    drop(store);

    let rt = Execra::open(config).await.unwrap();
    let job = rt.job(id).await.expect("stranded job should load");
    assert_eq!(job.state, JobState::Finalized);
    assert!(matches!(
        job.outcome,
        Some(Outcome::Failed {
            reason: execra::FailureReason::SpawnFailed { ref error },
            ..
        }) if error == "host process exited"
    ));
}

#[tokio::test]
async fn raw_output_policy_controls_flat_log_files() {
    let persist_config = test_config();
    let rt = Execra::open(persist_config.clone()).await.unwrap();
    let handle = rt.spawn(echo("raw line")).await.unwrap();
    let id = handle.id();
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
    let log = std::fs::read_to_string(persist_config.log_dir.join(format!("{id}.log"))).unwrap();
    assert!(log.contains("[stdout] raw line"));

    #[cfg(feature = "gzip")]
    {
        let mut gzip_config = test_config();
        gzip_config.raw_output = RawOutputPolicy::PersistGzipOnFinalize;
        let rt = Execra::open(gzip_config.clone()).await.unwrap();
        let handle = rt.spawn(echo("gzip line")).await.unwrap();
        let id = handle.id();
        let outcome = handle.await;
        assert!(
            matches!(outcome, Outcome::Succeeded { .. }),
            "got {outcome:?}"
        );
        assert!(!gzip_config.log_dir.join(format!("{id}.log")).exists());
        let gz = std::fs::File::open(gzip_config.log_dir.join(format!("{id}.log.gz"))).unwrap();
        let mut decoder = flate2::read::GzDecoder::new(gz);
        let mut log = String::new();
        decoder.read_to_string(&mut log).unwrap();
        assert!(log.contains("[stdout] gzip line"));
    }

    let mut disabled_config = test_config();
    disabled_config.raw_output = RawOutputPolicy::Disabled;
    let rt = Execra::open(disabled_config.clone()).await.unwrap();
    let handle = rt.spawn(echo("drop line")).await.unwrap();
    let id = handle.id();
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
    assert!(!disabled_config.log_dir.join(format!("{id}.log")).exists());
}

#[tokio::test]
async fn memory_only_raw_output_streams_live_without_flat_log() {
    let mut config = test_config();
    config.raw_output = RawOutputPolicy::MemoryOnly;
    let rt = Execra::open(config.clone()).await.unwrap();
    let mut handle = rt.spawn(echo("memory line")).await.unwrap();
    let id = handle.id();
    let mut events = handle.subscribe();
    let mut saw_output = false;
    while let Some(event) = events.next().await {
        match event {
            execra::Event::OutputAppended { line, .. } if line.contains("memory line") => {
                saw_output = true;
            }
            execra::Event::Finalized { .. } => break,
            _ => {}
        }
    }
    let outcome = handle.await;
    assert!(
        matches!(outcome, Outcome::Succeeded { .. }),
        "got {outcome:?}"
    );
    assert!(saw_output);
    assert!(!config.log_dir.join(format!("{id}.log")).exists());
}

#[tokio::test]
async fn higher_persisted_schema_version_is_rejected() {
    let config = test_config();
    let store = execra::store::Store::open(&config.db_path).await.unwrap();
    let id = JobId::new();
    let job = Job {
        id,
        command: Command::new("versioned").spec().clone(),
        created_at: SystemTime::now(),
        started_at: None,
        state: JobState::Finalized,
        current_phase: None,
        progress: Progress::Unknown,
        label: None,
        exit: None,
        outcome: Some(Outcome::Succeeded {
            summary: None,
            findings: vec![],
        }),
    };
    store.upsert_job(&job).await.unwrap();
    drop(store);

    let conn = rusqlite::Connection::open(&config.db_path).unwrap();
    conn.execute("UPDATE jobs SET schema_version = 1", [])
        .unwrap();
    drop(conn);

    let store = execra::store::Store::open(&config.db_path).await.unwrap();
    let err = store.load_job(id).await.unwrap_err();
    assert!(matches!(
        err,
        execra::store::StoreError::UnsupportedSchemaVersion {
            found: 1,
            supported: 0
        }
    ));
}

#[test]
fn command_hide_window_flag_is_stored() {
    assert!(Command::new("cmd").hide_window(true).spec().hide_window);
}

#[test]
fn command_helpers_build_platform_shells() {
    let shell = Command::shell("echo helper");
    #[cfg(windows)]
    {
        assert_eq!(shell.spec().program, "cmd");
        assert_eq!(shell.spec().args, vec!["/C", "echo helper"]);
        assert!(Command::new("cmd").spec().hide_window);
    }
    #[cfg(not(windows))]
    {
        assert_eq!(shell.spec().program, "sh");
        assert_eq!(shell.spec().args, vec!["-c", "echo helper"]);
        assert!(!Command::new("sh").spec().hide_window);
    }

    assert_eq!(
        Command::powershell("Write-Host ok").spec().program,
        "powershell"
    );
    assert_eq!(Command::pwsh("Write-Host ok").spec().program, "pwsh");
    assert_eq!(Command::cmd("echo ok").spec().args, vec!["/C", "echo ok"]);
    assert_eq!(Command::sh("echo ok").spec().args, vec!["-c", "echo ok"]);
}
