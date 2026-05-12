use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use execra::{
    Command, Config, Event, Execra, Job, JobId, JobState, Outcome, RawOutputPolicy, Stream,
};

#[derive(Debug, Parser)]
#[command(name = "execra", version, about = "Run and inspect Execra jobs")]
struct Cli {
    #[arg(long, global = true, default_value = "execra.db")]
    db: PathBuf,
    #[arg(long, global = true, default_value = "execra-logs")]
    log_dir: PathBuf,
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    Run(RunArgs),
    Ls(LsArgs),
    Logs(JobArgs),
    Tail(TailArgs),
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    timeout_ms: Option<u64>,
    #[arg(long)]
    no_raw_log: bool,
    #[arg(required = true, trailing_var_arg = true)]
    command: Vec<String>,
}

#[derive(Debug, Args)]
struct LsArgs {
    #[arg(long)]
    json: bool,
    #[arg(long)]
    tag: Option<String>,
    #[arg(long)]
    state: Option<String>,
    #[arg(long, default_value_t = 50)]
    limit: usize,
}

#[derive(Debug, Args)]
struct JobArgs {
    job: String,
}

#[derive(Debug, Args)]
struct TailArgs {
    #[arg(long)]
    json: bool,
    #[arg(long, default_value_t = 1000)]
    limit: usize,
    job: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = Config {
        db_path: cli.db,
        log_dir: cli.log_dir,
        ..Config::default()
    };

    match cli.command {
        CliCommand::Run(args) => run(config, args).await,
        CliCommand::Ls(args) => ls(config, args).await,
        CliCommand::Logs(args) => logs(config, args),
        CliCommand::Tail(args) => tail(config, args).await,
    }
}

async fn run(mut config: Config, args: RunArgs) -> Result<()> {
    if args.no_raw_log {
        config.raw_output = RawOutputPolicy::Disabled;
    }
    let (program, rest) = args
        .command
        .split_first()
        .context("run requires a program")?;
    let mut cmd = Command::new(program.clone()).args(rest.iter().cloned());
    if let Some(ms) = args.timeout_ms {
        cmd = cmd.timeout(Duration::from_millis(ms));
    }

    let rt = Execra::open(config).await?;
    let mut handle = rt.spawn(cmd).await?;
    let job_id = handle.id();
    let mut events = handle.subscribe();
    let mut finalized_seen = false;

    loop {
        tokio::select! {
            event = events.next() => {
                if let Some(event) = event {
                    finalized_seen |= matches!(event, Event::Finalized { .. });
                    print_event(&event, args.json)?;
                }
            }
            outcome = &mut handle => {
                if args.json {
                    if !finalized_seen {
                        print_event(&Event::Finalized {
                            job: job_id,
                            outcome,
                            at: std::time::SystemTime::now(),
                        }, true)?;
                    }
                } else {
                    print_outcome(&outcome);
                }
                break;
            }
        }
    }
    Ok(())
}

async fn ls(config: Config, args: LsArgs) -> Result<()> {
    let rt = Execra::open(config).await?;
    let mut query = rt.jobs().limit(args.limit);
    if let Some(tag) = args.tag {
        query = query.with_tag(tag);
    }
    if let Some(state) = args.state {
        query = query.with_state(parse_job_state(&state)?);
    }
    let jobs = query.run(&rt).await?;
    if args.json {
        for job in jobs {
            println!("{}", serde_json::to_string(&job)?);
        }
    } else {
        for job in jobs {
            print_job(&job);
        }
    }
    Ok(())
}

fn logs(config: Config, args: JobArgs) -> Result<()> {
    let id = parse_job_id(&args.job)?;
    let plain = config.log_dir.join(format!("{id}.log"));
    let gz = config.log_dir.join(format!("{id}.log.gz"));
    if plain.exists() {
        print!("{}", std::fs::read_to_string(plain)?);
        return Ok(());
    }
    if gz.exists() {
        let file = std::fs::File::open(gz)?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut text = String::new();
        decoder.read_to_string(&mut text)?;
        print!("{text}");
        return Ok(());
    }
    bail!("no raw log found for job {id}");
}

async fn tail(config: Config, args: TailArgs) -> Result<()> {
    let id = parse_job_id(&args.job)?;
    let store = execra::store::Store::open(&config.db_path).await?;
    let events = store.list_events(id, args.limit).await?;
    for event in events {
        if args.json {
            println!("{}", serde_json::to_string(&event)?);
        } else {
            println!("{event:?}");
        }
    }
    Ok(())
}

fn parse_job_id(id: &str) -> Result<JobId> {
    Ok(JobId(uuid::Uuid::parse_str(id)?))
}

fn parse_job_state(state: &str) -> Result<JobState> {
    match state {
        "queued" => Ok(JobState::Queued),
        "running" => Ok(JobState::Running),
        "exited" => Ok(JobState::Exited),
        "finalized" => Ok(JobState::Finalized),
        "cancelled" => Ok(JobState::Cancelled),
        other => bail!("unknown job state: {other}"),
    }
}

fn print_event(event: &Event, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(event)?);
        return Ok(());
    }
    if let Event::OutputAppended { stream, line, .. } = event {
        match stream {
            Stream::Stdout => println!("{line}"),
            Stream::Stderr => eprintln!("{line}"),
        }
    }
    Ok(())
}

fn print_outcome(outcome: &Outcome) {
    eprintln!("outcome: {outcome:?}");
}

fn print_job(job: &Job) {
    let label = job.label.as_deref().unwrap_or("-");
    println!("{}\t{:?}\t{}", job.id, job.state, label);
}
