#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::SocketAddr;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use clap::{Args, Parser, Subcommand, ValueEnum};
use mumble_server_runtime_gateway::tls::Identity;
use mumble_server_runtime_stress::{
    Config as MumbleConfig, ManagedClientConfig, MumbleCredential, ScenarioKind, VoiceClip,
    spawn_managed,
};
use mumble_spaces_server::{ControllerConfig, RunningControllerServer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::process::{Child, ChildStdin, Command as ProcessCommand};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout};

const DRIVER_START_DEADLINE: Duration = Duration::from_secs(120);
const OPERATION_DEADLINE: Duration = Duration::from_secs(20);
const MAX_CLIENTS_PER_WORKER: usize = 512;
const PROTOCOL_VERSION: u8 = 1;

#[derive(Debug, Parser)]
#[command(about = "Headless Spaces load and resilience coordinator")]
pub struct Command {
    #[command(subcommand)]
    command: Subcommands,
}

#[derive(Debug, Subcommand)]
enum Subcommands {
    Run(RunArguments),
    #[command(hide = true)]
    Worker(WorkerArguments),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Managed,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scenario {
    ControlOnly,
    Idle,
    FixedSpaces,
    OneSpace,
    ManySpaces,
    Churn,
    Migration,
    MuteDeaf,
    Voice,
}

#[derive(Debug, Args)]
struct RunArguments {
    #[arg(long, value_enum, default_value = "managed")]
    mode: Mode,
    #[arg(long)]
    controller_endpoint: Option<String>,
    #[arg(long)]
    mumble_server: Option<SocketAddr>,
    #[arg(
        long,
        default_value = "implementations/spaces/tools/load-driver-java/build/install/load-driver-java/bin/load-driver-java"
    )]
    driver: PathBuf,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=64))]
    controllers: u16,
    #[arg(long, default_value_t = 8, value_parser = parse_participants)]
    participants: usize,
    #[arg(long, default_value_t = 8, value_parser = parse_participants_per_space)]
    participants_per_space: usize,
    #[arg(long, value_enum, default_value = "idle")]
    scenario: Scenario,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    duration: Duration,
    #[arg(long, default_value = "0s", value_parser = parse_duration)]
    ramp: Duration,
    #[arg(long)]
    voice_file: Option<PathBuf>,
    #[arg(long, default_value = "spaces-stress-results")]
    result_root: PathBuf,
}

#[derive(Debug, Args)]
struct WorkerArguments {
    #[arg(long)]
    mumble_server: SocketAddr,
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    duration: Duration,
    #[arg(long, default_value = "0s", value_parser = parse_duration)]
    ramp: Duration,
    #[arg(long)]
    voice_file: Option<PathBuf>,
    #[arg(long, default_value_t = 5)]
    talk_percent: u8,
}

#[derive(Debug, Deserialize)]
struct WorkerCredential {
    participant_id: String,
    credential: String,
}

#[derive(Debug, Serialize)]
struct Manifest<'a> {
    schema_version: u8,
    git_sha: String,
    git_dirty: bool,
    operating_system: &'static str,
    architecture: &'static str,
    available_parallelism: usize,
    mode: Mode,
    scenario: Scenario,
    controllers: u16,
    participants: usize,
    participants_per_space: usize,
    seed: u64,
    command_line: &'a [OsString],
}

#[derive(Debug, Default, Serialize)]
struct Summary {
    scenario: String,
    controllers: u16,
    participants_requested: usize,
    credentials_received: usize,
    workers: usize,
    driver_events: u64,
    credential_rotations: u64,
    errors: u64,
    elapsed_millis: u64,
}

#[derive(Debug, Clone)]
struct ParticipantPlan {
    participant_id: String,
    display_name: String,
    space_key: String,
    controller: usize,
}

#[derive(Debug, Default)]
struct Ledger {
    active_sessions: BTreeSet<String>,
    expected_spaces: BTreeSet<String>,
    participants: BTreeMap<String, LedgerParticipant>,
}

#[derive(Debug)]
struct LedgerParticipant {
    expected_controller: String,
    desired_space: String,
    owned: bool,
    credential_rotations: u64,
    connected: bool,
    applied_space: String,
    accepted_revision: String,
    applied_revision: String,
    published_generation: String,
}

struct Driver {
    controller_id: String,
    input: ChildStdin,
    child: Child,
    reader: JoinHandle<()>,
}

struct ManagedEnvironment {
    server: Option<RunningControllerServer>,
    controller_endpoint: String,
    mumble_server: SocketAddr,
}

#[derive(Debug, Error)]
enum CampaignError {
    #[error("external mode requires both --controller-endpoint and --mumble-server")]
    MissingExternalEndpoints,
    #[error("managed mode allocates at most {0} Mumble connections")]
    TooManyManagedConnections(usize),
}

pub async fn run(command: Command) -> Result<()> {
    match command.command {
        Subcommands::Run(arguments) => run_campaign(arguments).await,
        Subcommands::Worker(arguments) => run_worker(arguments).await,
    }
}

async fn run_campaign(arguments: RunArguments) -> Result<()> {
    validate_run_arguments(&arguments)?;
    let started = Instant::now();
    let result_directory = create_result_directory(&arguments)?;
    write_manifest(&result_directory, &arguments)?;
    let mut events = event_writer(&result_directory)?;
    let mut server_log = BufWriter::new(File::create(result_directory.join("server.log"))?);
    let environment = start_environment(&arguments).await?;
    writeln!(
        server_log,
        "controller_endpoint={} mumble_server={}",
        environment.controller_endpoint, environment.mumble_server
    )?;
    server_log.flush()?;

    let (driver_events, mut event_receiver) = mpsc::channel(8192);
    let mut drivers = spawn_drivers(
        &arguments,
        &environment.controller_endpoint,
        &result_directory,
        driver_events,
    )
    .await?;
    let plans = participant_plans(
        arguments.participants,
        arguments.participants_per_space,
        usize::from(arguments.controllers),
        arguments.scenario,
        arguments.seed,
    );
    let mut summary = Summary {
        scenario: format!("{:?}", arguments.scenario),
        controllers: arguments.controllers,
        participants_requested: plans.len(),
        ..Summary::default()
    };
    let mut ledger = Ledger::from_plans(&plans);

    for plan in &plans {
        send_driver_command(
            drivers
                .get_mut(plan.controller)
                .context("driver assignment")?,
            json!({
                "schema_version": PROTOCOL_VERSION,
                "kind": "register",
                "correlation_id": format!("register-{}", plan.participant_id),
                "participant_id": plan.participant_id,
                "space_key": plan.space_key,
                "display_name": plan.display_name,
                "server_mute": false,
                "server_deaf": false,
            }),
        )
        .await?;
    }

    let mut credentials = BTreeMap::new();
    if !matches!(arguments.scenario, Scenario::ControlOnly) {
        collect_credentials(
            &plans,
            &mut credentials,
            &mut event_receiver,
            &mut events,
            &mut summary,
            &mut ledger,
        )
        .await?;
    } else {
        drain_until_owned(
            plans.len(),
            &mut event_receiver,
            &mut events,
            &mut summary,
            &mut ledger,
        )
        .await?;
    }

    let mut workers = if credentials.is_empty() {
        Vec::new()
    } else {
        spawn_workers(
            &arguments,
            environment.mumble_server,
            &result_directory,
            credentials,
        )
        .await?
    };
    summary.credentials_received = plans.len().min(summary.credentials_received);
    summary.workers = workers.len();
    let recorder = tokio::spawn(async move {
        while let Some((_, event)) = event_receiver.recv().await {
            record_driver_event(
                event,
                &mut BTreeMap::new(),
                &mut events,
                &mut summary,
                &mut ledger,
            )?;
        }
        Ok::<_, anyhow::Error>((summary, ledger))
    });

    tokio::time::sleep(arguments.ramp + Duration::from_millis(250)).await;
    apply_scenario(&arguments, &plans, &mut drivers).await?;
    for worker in &mut workers {
        let status = worker.wait().await.context("waiting for Mumble worker")?;
        ensure!(status.success(), "Mumble worker exited with {status}");
    }

    for driver in &mut drivers {
        send_driver_command(
            driver,
            json!({
                "schema_version": PROTOCOL_VERSION,
                "kind": "shutdown",
                "correlation_id": "shutdown",
            }),
        )
        .await?;
        let status = timeout(OPERATION_DEADLINE, driver.child.wait())
            .await
            .context("driver shutdown timeout")??;
        ensure!(
            status.success(),
            "driver {} exited with {status}",
            driver.controller_id
        );
        (&mut driver.reader)
            .await
            .context("joining Java driver output reader")?;
    }
    if let Some(server) = environment.server {
        server.shutdown().await;
    }
    let (mut summary, ledger) = recorder.await.context("joining driver event recorder")??;
    audit_ledger(&plans, &ledger)?;
    summary.elapsed_millis = duration_millis(started.elapsed());
    write_summary(&result_directory, &summary)?;
    Ok(())
}

fn validate_run_arguments(arguments: &RunArguments) -> Result<()> {
    ensure!(
        arguments
            .participants
            .is_multiple_of(arguments.participants_per_space),
        "--participants must be divisible by --participants-per-space"
    );
    ensure!(
        arguments.participants >= arguments.participants_per_space,
        "at least one full Space is required"
    );
    if matches!(arguments.mode, Mode::External)
        && (arguments.controller_endpoint.is_none() || arguments.mumble_server.is_none())
    {
        return Err(CampaignError::MissingExternalEndpoints.into());
    }
    if matches!(arguments.mode, Mode::Managed) && arguments.participants > 10_000 {
        return Err(CampaignError::TooManyManagedConnections(arguments.participants).into());
    }
    Ok(())
}

async fn start_environment(arguments: &RunArguments) -> Result<ManagedEnvironment> {
    if matches!(arguments.mode, Mode::External) {
        return Ok(ManagedEnvironment {
            server: None,
            controller_endpoint: arguments
                .controller_endpoint
                .clone()
                .context("validated controller endpoint")?,
            mumble_server: arguments.mumble_server.context("validated Mumble server")?,
        });
    }
    let identity = Identity::self_signed(vec!["localhost".to_owned()])
        .context("generating managed Mumble identity")?;
    let maximum_connections = u32::try_from(arguments.participants.max(1))
        .context("managed connection limit exceeds u32")?;
    let config = ControllerConfig {
        controller_bind: "127.0.0.1:0".parse()?,
        mumble_bind: "127.0.0.1:0".parse()?,
        max_sessions: usize::from(arguments.controllers).max(1),
        max_participants: arguments.participants.max(1),
        max_participants_per_session: arguments.participants.max(1),
        max_spaces: arguments.participants.max(1),
        max_observations_per_session: arguments.participants.max(1),
        queue_capacity: arguments.participants.saturating_mul(2).max(1024),
        max_mumble_connections: maximum_connections,
        ..ControllerConfig::default()
    };
    let server = RunningControllerServer::start(config, identity).await?;
    let controller_endpoint = format!("http://{}", server.controller_address());
    let mumble_server = server.mumble_address();
    Ok(ManagedEnvironment {
        server: Some(server),
        controller_endpoint,
        mumble_server,
    })
}

async fn spawn_drivers(
    arguments: &RunArguments,
    endpoint: &str,
    result_directory: &Path,
    events: mpsc::Sender<(usize, Value)>,
) -> Result<Vec<Driver>> {
    let mut drivers = Vec::new();
    for index in 0..usize::from(arguments.controllers) {
        let controller_id = format!("load-controller-{index}");
        let stderr = File::create(result_directory.join(format!("driver-{index}.log")))?;
        let mut child = ProcessCommand::new(&arguments.driver)
            .arg(endpoint)
            .arg(&controller_id)
            .arg("512")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting Java driver {}", arguments.driver.display()))?;
        let input = child.stdin.take().context("Java driver stdin")?;
        let output = child.stdout.take().context("Java driver stdout")?;
        let events = events.clone();
        let reader = tokio::spawn(async move {
            let mut lines = AsyncBufReader::new(output).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => match serde_json::from_str::<Value>(&line) {
                        Ok(event) => {
                            if events.send((index, event)).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Ok(None) | Err(_) => return,
                }
            }
        });
        drivers.push(Driver {
            controller_id,
            input,
            child,
            reader,
        });
    }
    drop(events);
    Ok(drivers)
}

async fn send_driver_command(driver: &mut Driver, value: Value) -> Result<()> {
    let mut encoded = serde_json::to_vec(&value)?;
    encoded.push(b'\n');
    driver.input.write_all(&encoded).await?;
    driver.input.flush().await?;
    Ok(())
}

async fn collect_credentials(
    plans: &[ParticipantPlan],
    credentials: &mut BTreeMap<String, String>,
    events: &mut mpsc::Receiver<(usize, Value)>,
    output: &mut BufWriter<File>,
    summary: &mut Summary,
    ledger: &mut Ledger,
) -> Result<()> {
    let expected: BTreeSet<&str> = plans
        .iter()
        .map(|plan| plan.participant_id.as_str())
        .collect();
    timeout(DRIVER_START_DEADLINE, async {
        while credentials.len() < expected.len() {
            let (_, event) = events.recv().await.context("all Java drivers stopped")?;
            record_driver_event(event, credentials, output, summary, ledger)?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("credential collection timeout")??;
    Ok(())
}

async fn drain_until_owned(
    expected: usize,
    events: &mut mpsc::Receiver<(usize, Value)>,
    output: &mut BufWriter<File>,
    summary: &mut Summary,
    ledger: &mut Ledger,
) -> Result<()> {
    let mut owned = BTreeSet::new();
    timeout(DRIVER_START_DEADLINE, async {
        while owned.len() < expected {
            let (_, event) = events.recv().await.context("all Java drivers stopped")?;
            if event.get("kind").and_then(Value::as_str) == Some("owned")
                && let Some(participant) = event.get("participant_id").and_then(Value::as_str)
            {
                owned.insert(participant.to_owned());
            }
            record_driver_event(event, &mut BTreeMap::new(), output, summary, ledger)?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("ownership collection timeout")??;
    Ok(())
}

fn record_driver_event(
    mut event: Value,
    credentials: &mut BTreeMap<String, String>,
    output: &mut BufWriter<File>,
    summary: &mut Summary,
    ledger: &mut Ledger,
) -> Result<()> {
    summary.driver_events = summary.driver_events.saturating_add(1);
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind == "credential" {
        let participant = event
            .get("participant_id")
            .and_then(Value::as_str)
            .context("credential participant_id")?
            .to_owned();
        let credential_value = event
            .get_mut("credential")
            .map(Value::take)
            .context("credential value")?;
        let credential = credential_value
            .as_str()
            .context("credential value")?
            .to_owned();
        credentials.insert(participant, credential);
        summary.credentials_received = credentials.len();
        summary.credential_rotations = summary.credential_rotations.saturating_add(1);
        event["kind"] = Value::String("credential_rotated".to_owned());
        if let Some(object) = event.as_object_mut() {
            object.remove("credential");
        }
    } else if kind == "error" {
        summary.errors = summary.errors.saturating_add(1);
    }
    ledger.record(&event);
    if let Some(object) = event.as_object_mut() {
        object.insert(
            "coordinator_unix_nanos".to_owned(),
            Value::String(unix_nanos().to_string()),
        );
    }
    serde_json::to_writer(&mut *output, &event)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

async fn spawn_workers(
    arguments: &RunArguments,
    mumble_server: SocketAddr,
    result_directory: &Path,
    credentials: BTreeMap<String, String>,
) -> Result<Vec<Child>> {
    let executable = std::env::current_exe()?;
    let entries: Vec<_> = credentials.into_iter().collect();
    let mut workers = Vec::new();
    for (index, chunk) in entries.chunks(MAX_CLIENTS_PER_WORKER).enumerate() {
        let stdout = File::create(result_directory.join(format!("worker-{index}.jsonl")))?;
        let stderr = File::create(result_directory.join(format!("worker-{index}.log")))?;
        let mut command = ProcessCommand::new(&executable);
        command
            .arg("worker")
            .arg("--mumble-server")
            .arg(mumble_server.to_string())
            .arg("--duration")
            .arg(format!("{}ms", arguments.duration.as_millis()))
            .arg("--ramp")
            .arg(format!("{}ms", arguments.ramp.as_millis()))
            .stdin(Stdio::piped())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true);
        if matches!(arguments.scenario, Scenario::Voice)
            && let Some(path) = &arguments.voice_file
        {
            command.arg("--voice-file").arg(path);
        }
        let mut child = command.spawn().context("starting Mumble worker")?;
        let mut input = child.stdin.take().context("Mumble worker stdin")?;
        for (participant_id, credential) in chunk {
            let line = serde_json::to_vec(&WorkerCredential {
                participant_id: participant_id.clone(),
                credential: credential.clone(),
            })?;
            input.write_all(&line).await?;
            input.write_all(b"\n").await?;
        }
        input.shutdown().await?;
        workers.push(child);
    }
    Ok(workers)
}

async fn run_worker(arguments: WorkerArguments) -> Result<()> {
    ensure!(
        arguments.talk_percent <= 100,
        "--talk-percent must be at most 100"
    );
    let voice_clip = arguments
        .voice_file
        .as_deref()
        .map(|path| VoiceClip::load(path, NonZeroUsize::new(30).context("30 is nonzero")?))
        .transpose()?
        .map(Arc::new);
    let stdin = std::io::stdin();
    let mut credentials = Vec::new();
    for line in BufReader::new(stdin.lock()).lines() {
        credentials.push(serde_json::from_str::<WorkerCredential>(&line?)?);
    }
    ensure!(
        credentials.len() <= MAX_CLIENTS_PER_WORKER,
        "worker credential limit exceeded"
    );
    let clients = NonZeroUsize::new(credentials.len()).context("worker received no credentials")?;
    let mut tasks = JoinSet::new();
    for (index, credential) in credentials.into_iter().enumerate() {
        let config = MumbleConfig {
            server: arguments.mumble_server,
            clients,
            scenario: ScenarioKind::Connect,
            ramp: arguments.ramp,
            duration: arguments.duration,
            connect_timeout: Duration::from_secs(5),
            handshake_timeout: Duration::from_secs(10),
            ping_interval: Duration::from_secs(5),
            voice_file: arguments.voice_file.clone(),
            talk_percent: arguments.talk_percent,
            talk_spurt: Duration::from_secs(2),
            voice_frame_bytes: NonZeroUsize::new(30).context("30 is nonzero")?,
            interaction_interval: Duration::from_secs(1),
            username_prefix: "spaces-load".to_owned(),
            password: None,
            failure_threshold: 0.0,
            json_output: None,
        };
        let client = spawn_managed(ManagedClientConfig {
            config,
            credential: MumbleCredential::new(&credential.participant_id, credential.credential),
            client_number: index,
            voice_clip: voice_clip.as_ref().map(Arc::clone),
            voice_enabled: voice_clip.is_some(),
        })?;
        tasks.spawn(async move {
            let mut client = client;
            while let Some(event) = client.next_event().await {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "participant_id": credential.participant_id,
                        "observed_unix_nanos": unix_nanos().to_string(),
                        "event": event,
                    }))?
                );
            }
            let report = client.wait().await?;
            Ok::<_, anyhow::Error>(report)
        });
    }
    while let Some(result) = tasks.join_next().await {
        let report = result??;
        ensure!(
            report.completed,
            "Mumble client did not complete: {:?}",
            report.error
        );
    }
    Ok(())
}

async fn apply_scenario(
    arguments: &RunArguments,
    plans: &[ParticipantPlan],
    drivers: &mut [Driver],
) -> Result<()> {
    match arguments.scenario {
        Scenario::Migration => {
            for plan in plans {
                send_spec(
                    drivers,
                    plan,
                    format!("{}-migrated", plan.space_key),
                    false,
                    false,
                )
                .await?;
            }
        }
        Scenario::MuteDeaf => {
            for (index, plan) in plans.iter().enumerate() {
                send_spec(
                    drivers,
                    plan,
                    plan.space_key.clone(),
                    index.is_multiple_of(2),
                    index.is_multiple_of(3),
                )
                .await?;
            }
        }
        Scenario::Churn => {
            for plan in plans.iter().step_by(4) {
                send_driver_command(
                    drivers
                        .get_mut(plan.controller)
                        .context("driver assignment")?,
                    json!({
                        "schema_version": PROTOCOL_VERSION,
                        "kind": "release",
                        "correlation_id": format!("release-{}", plan.participant_id),
                        "participant_id": plan.participant_id,
                    }),
                )
                .await?;
            }
        }
        _ => {}
    }
    Ok(())
}

async fn send_spec(
    drivers: &mut [Driver],
    plan: &ParticipantPlan,
    space_key: String,
    server_mute: bool,
    server_deaf: bool,
) -> Result<()> {
    send_driver_command(
        drivers
            .get_mut(plan.controller)
            .context("driver assignment")?,
        json!({
            "schema_version": PROTOCOL_VERSION,
            "kind": "set_spec",
            "correlation_id": format!("spec-{}", plan.participant_id),
            "participant_id": plan.participant_id,
            "space_key": space_key,
            "display_name": plan.display_name,
            "server_mute": server_mute,
            "server_deaf": server_deaf,
        }),
    )
    .await
}

fn participant_plans(
    participants: usize,
    participants_per_space: usize,
    controllers: usize,
    scenario: Scenario,
    seed: u64,
) -> Vec<ParticipantPlan> {
    (0..participants)
        .map(|index| {
            let space = match scenario {
                Scenario::OneSpace => 0,
                Scenario::ManySpaces => index,
                _ => index / participants_per_space,
            };
            let shuffled = index.wrapping_add(usize::try_from(seed).unwrap_or(0));
            ParticipantPlan {
                participant_id: format!("load-participant-{index}"),
                display_name: format!("Load participant {index}"),
                space_key: format!("load-space-{space}"),
                controller: shuffled % controllers,
            }
        })
        .collect()
}

fn create_result_directory(arguments: &RunArguments) -> Result<PathBuf> {
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let path = arguments.result_root.join(format!(
        "{timestamp}-{:?}-{}",
        arguments.scenario, arguments.seed
    ));
    std::fs::create_dir_all(&path)?;
    Ok(path)
}

fn write_manifest(directory: &Path, arguments: &RunArguments) -> Result<()> {
    let git_sha = command_output("git", &["rev-parse", "HEAD"])?;
    let git_dirty = !command_output("git", &["status", "--porcelain"])?.is_empty();
    let command_line: Vec<_> = std::env::args_os().collect();
    let manifest = Manifest {
        schema_version: PROTOCOL_VERSION,
        git_sha,
        git_dirty,
        operating_system: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        available_parallelism: std::thread::available_parallelism()?.get(),
        mode: arguments.mode,
        scenario: arguments.scenario,
        controllers: arguments.controllers,
        participants: arguments.participants,
        participants_per_space: arguments.participants_per_space,
        seed: arguments.seed,
        command_line: &command_line,
    };
    serde_json::to_writer_pretty(File::create(directory.join("manifest.json"))?, &manifest)?;
    Ok(())
}

fn write_summary(directory: &Path, summary: &Summary) -> Result<()> {
    serde_json::to_writer_pretty(File::create(directory.join("summary.json"))?, summary)?;
    let mut csv = BufWriter::new(File::create(directory.join("summary.csv"))?);
    writeln!(
        csv,
        "scenario,controllers,participants,credentials,workers,driver_events,credential_rotations,errors,elapsed_millis"
    )?;
    writeln!(
        csv,
        "{},{},{},{},{},{},{},{},{}",
        summary.scenario,
        summary.controllers,
        summary.participants_requested,
        summary.credentials_received,
        summary.workers,
        summary.driver_events,
        summary.credential_rotations,
        summary.errors,
        summary.elapsed_millis
    )?;
    Ok(())
}

fn event_writer(directory: &Path) -> Result<BufWriter<File>> {
    Ok(BufWriter::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(directory.join("events.jsonl"))?,
    ))
}

fn command_output(program: &str, arguments: &[&str]) -> Result<String> {
    let output = std::process::Command::new(program)
        .args(arguments)
        .output()?;
    ensure!(
        output.status.success(),
        "{program} exited with {}",
        output.status
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn parse_participants_per_space(value: &str) -> Result<usize, String> {
    let parsed = value.parse::<usize>().map_err(|error| error.to_string())?;
    if [8, 32, 64, 128, 256].contains(&parsed) {
        Ok(parsed)
    } else {
        Err("participants per Space must be 8, 32, 64, 128, or 256".to_owned())
    }
}

fn parse_participants(value: &str) -> Result<usize, String> {
    let parsed = value.parse::<usize>().map_err(|error| error.to_string())?;
    if (1..=10_000).contains(&parsed) {
        Ok(parsed)
    } else {
        Err("participants must be between 1 and 10000".to_owned())
    }
}

fn parse_duration(value: &str) -> Result<Duration, String> {
    let (number, multiplier) = if let Some(number) = value.strip_suffix("ms") {
        (number, 1)
    } else if let Some(number) = value.strip_suffix('s') {
        (number, 1_000)
    } else if let Some(number) = value.strip_suffix('m') {
        (number, 60_000)
    } else {
        return Err("use a duration suffix: ms, s, or m".to_owned());
    };
    let amount = number.parse::<u64>().map_err(|error| error.to_string())?;
    amount
        .checked_mul(multiplier)
        .map(Duration::from_millis)
        .ok_or_else(|| "duration is too large".to_owned())
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

impl Ledger {
    fn from_plans(plans: &[ParticipantPlan]) -> Self {
        let mut ledger = Self::default();
        for plan in plans {
            ledger.expected_spaces.insert(plan.space_key.clone());
            ledger.participants.insert(
                plan.participant_id.clone(),
                LedgerParticipant {
                    expected_controller: format!("load-controller-{}", plan.controller),
                    desired_space: plan.space_key.clone(),
                    owned: false,
                    credential_rotations: 0,
                    connected: false,
                    applied_space: String::new(),
                    accepted_revision: String::new(),
                    applied_revision: String::new(),
                    published_generation: String::new(),
                },
            );
        }
        ledger
    }

    fn record(&mut self, event: &Value) {
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
        let controller = event
            .get("controller_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if kind == "session_state" && event.get("current").and_then(Value::as_str) == Some("ACTIVE")
        {
            self.active_sessions.insert(controller.to_owned());
        }
        let Some(participant_id) = event.get("participant_id").and_then(Value::as_str) else {
            return;
        };
        let Some(participant) = self.participants.get_mut(participant_id) else {
            return;
        };
        match kind {
            "owned" => participant.owned = true,
            "credential_rotated" => {
                participant.credential_rotations =
                    participant.credential_rotations.saturating_add(1);
            }
            "accepted" | "participant_status" => {
                participant.accepted_revision = string_field(event, "accepted_spec_revision");
                participant.applied_revision = string_field(event, "applied_spec_revision");
                participant.published_generation = string_field(event, "published_generation");
                if kind == "participant_status" {
                    participant.connected = event
                        .get("connected")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    participant.applied_space = string_field(event, "space_key");
                }
            }
            _ => {}
        }
    }
}

fn audit_ledger(plans: &[ParticipantPlan], ledger: &Ledger) -> Result<()> {
    ensure!(
        ledger.participants.len() == plans.len(),
        "ledger participant count diverged"
    );
    for plan in plans {
        let participant = ledger
            .participants
            .get(&plan.participant_id)
            .context("participant missing from ledger")?;
        ensure!(
            participant.expected_controller == format!("load-controller-{}", plan.controller),
            "ledger controller assignment diverged for {}",
            plan.participant_id
        );
        ensure!(participant.owned, "{} was never owned", plan.participant_id);
        ensure!(
            participant.credential_rotations > 0,
            "{} never received a credential",
            plan.participant_id
        );
        ensure!(
            participant.desired_space == plan.space_key,
            "ledger desired Space diverged for {}",
            plan.participant_id
        );
    }
    Ok(())
}

fn string_field(event: &Value, name: &str) -> String {
    event
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

impl Serialize for WorkerCredential {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        #[derive(Serialize)]
        struct Wire<'a> {
            participant_id: &'a str,
            credential: &'a str,
        }
        Wire {
            participant_id: &self.participant_id,
            credential: &self.credential,
        }
        .serialize(serializer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_spaces_are_full_and_seeded_assignment_is_repeatable() {
        let first = participant_plans(64, 8, 4, Scenario::FixedSpaces, 17);
        let second = participant_plans(64, 8, 4, Scenario::FixedSpaces, 17);

        assert_eq!(
            first.iter().map(|plan| &plan.space_key).collect::<Vec<_>>(),
            second
                .iter()
                .map(|plan| &plan.space_key)
                .collect::<Vec<_>>()
        );
        let counts = first.iter().fold(BTreeMap::new(), |mut counts, plan| {
            *counts.entry(&plan.space_key).or_insert(0usize) += 1;
            counts
        });
        assert!(counts.values().all(|count| *count == 8));
    }

    #[test]
    fn only_documented_space_cardinalities_are_accepted() {
        for allowed in [8, 32, 64, 128, 256] {
            assert_eq!(
                parse_participants_per_space(&allowed.to_string()),
                Ok(allowed)
            );
        }
        assert!(parse_participants_per_space("16").is_err());
    }

    #[test]
    fn persisted_credential_event_is_redacted() -> Result<()> {
        let mut event = json!({
            "kind": "credential",
            "participant_id": "p1",
            "credential": "secret-token"
        });
        let mut credentials = BTreeMap::new();
        let mut summary = Summary::default();
        let mut ledger = Ledger::default();
        let temporary =
            std::env::temp_dir().join(format!("mumble-spaces-stress-test-{}", std::process::id()));
        let file = File::create(&temporary)?;
        record_driver_event(
            std::mem::take(&mut event),
            &mut credentials,
            &mut BufWriter::new(file),
            &mut summary,
            &mut ledger,
        )?;
        let persisted = std::fs::read_to_string(&temporary)?;
        std::fs::remove_file(&temporary)?;
        assert_eq!(
            credentials.get("p1").map(String::as_str),
            Some("secret-token")
        );
        assert!(!persisted.contains("secret-token"));
        assert!(!persisted.contains("\"credential\""));
        Ok(())
    }
}
