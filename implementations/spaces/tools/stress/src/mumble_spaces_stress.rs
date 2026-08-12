#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{IpAddr, SocketAddr};
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
use mumble_spaces_server::{ControllerConfig, MetricsOutput, RunningControllerServer};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::process::{Child, ChildStdin, Command as ProcessCommand};
use tokio::sync::{mpsc, oneshot};
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
    Matrix(MatrixArguments),
    #[command(hide = true)]
    Worker(WorkerArguments),
}

#[derive(Debug, Args)]
struct MatrixArguments {
    #[arg(long, default_value = "spaces-load-matrix.json")]
    output: PathBuf,
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
    MultiSharedSpaces,
    MultiIsolatedSpaces,
    #[value(name = "skew-90-10")]
    #[serde(rename = "skew-90-10")]
    Skew90_10,
    BatchTakeover,
    SimultaneousClaim,
    CrossedTakeover,
    PingPong,
    StaleReconnect,
    IdentityBoundary,
    OverloadRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessFault {
    None,
    GracefulStop,
    Crash,
    FreezeShort,
    FreezeLong,
    HostRestart,
    MumbleCut,
    ControlledLimit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailurePhase {
    Stable,
    Injection,
    DegradedLoad,
    Healing,
    Reconciliation,
    FinalAudit,
}

#[derive(Debug, Args)]
struct RunArguments {
    #[arg(long, value_enum, default_value = "managed")]
    mode: Mode,
    #[arg(long)]
    controller_endpoint: Option<String>,
    #[arg(long)]
    mumble_server: Option<SocketAddr>,
    #[arg(long, default_value = "127.0.0.1")]
    managed_bind_ip: IpAddr,
    #[arg(
        long,
        default_value = "implementations/spaces/tools/load-driver-java/build/install/load-driver-java/bin/load-driver-java"
    )]
    driver: PathBuf,
    #[arg(long)]
    driver_netns_prefix: Option<String>,
    #[arg(long)]
    worker_netns_prefix: Option<String>,
    #[arg(long)]
    phase_control: Option<PathBuf>,
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..=64))]
    controllers: u16,
    #[arg(long, default_value_t = 8, value_parser = parse_participants)]
    participants: usize,
    #[arg(long, default_value_t = 8, value_parser = parse_participants_per_space)]
    participants_per_space: usize,
    #[arg(long, value_enum, default_value = "idle")]
    scenario: Scenario,
    #[arg(long, value_enum)]
    audio: Option<AudioLevel>,
    #[arg(long, default_value_t = 1)]
    seed: u64,
    #[arg(long, default_value = "30s", value_parser = parse_duration)]
    duration: Duration,
    #[arg(long, default_value = "0s", value_parser = parse_duration)]
    ramp: Duration,
    #[arg(long, value_enum, default_value = "none")]
    fault: ProcessFault,
    #[arg(long, default_value = "2s", value_parser = parse_duration)]
    fault_duration: Duration,
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
    voice_enabled: bool,
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
    audio: Option<AudioLevel>,
    fault: ProcessFault,
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
    ownership_losses: u64,
    ownership_violations: u64,
    stale_credential_rejections: u64,
    recovery_audits: u64,
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
    seen_credentials: BTreeSet<String>,
    credential_reuse_violations: u64,
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
    current_owner: String,
    previous_owner: String,
    ownership_losses: u64,
}

#[derive(Debug)]
struct CredentialRotation {
    participant_id: String,
    credential: String,
}

#[derive(Debug, Default)]
struct ScenarioOutcome {
    expected_rotations: usize,
}

#[derive(Debug, Serialize)]
struct LoadMatrix {
    schema_version: u8,
    points: Vec<LoadPoint>,
    fixed_totals: [usize; 2],
    resilience_capacity_percent: [u8; 3],
    full_fault_cardinalities: [usize; 2],
    edge_fault_cardinalities: [usize; 2],
    healthy_thresholds: HealthyThresholds,
}

#[derive(Debug, Serialize)]
struct LoadPoint {
    participants_per_space: usize,
    participants: usize,
    spaces: usize,
    audio: AudioLevel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum AudioLevel {
    None,
    OnePerSpace,
    FivePercent,
}

#[derive(Debug, Serialize)]
struct HealthyThresholds {
    maximum_server_cpu_percent: u8,
    maximum_generator_cpu_percent: u8,
    synchronization_p99_millis: u16,
    migration_p99_millis: u16,
    mute_p99_millis: u16,
    ping_p99_millis: u16,
    minimum_audio_delivery_percent: f64,
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

struct ProcessSampler {
    stop: oneshot::Sender<()>,
    task: JoinHandle<Result<()>>,
}

impl ProcessSampler {
    async fn stop(self) -> Result<()> {
        let _receiver_gone = self.stop.send(());
        self.task
            .await
            .context("joining process metrics sampler")??;
        Ok(())
    }
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
        Subcommands::Matrix(arguments) => write_load_matrix(&arguments.output),
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
    let environment = start_environment(&arguments, &result_directory).await?;
    writeln!(
        server_log,
        "controller_endpoint={} mumble_server={}",
        environment.controller_endpoint, environment.mumble_server
    )?;
    server_log.flush()?;

    let (driver_events, mut event_receiver) = mpsc::channel(8192);
    let coordinator_events = driver_events.clone();
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

    let known_credentials = credentials.clone();
    let mut workers = if credentials.is_empty() {
        Vec::new()
    } else {
        spawn_workers(
            &arguments,
            environment.mumble_server,
            &result_directory,
            credentials,
            0,
        )
        .await?
    };
    summary.credentials_received = plans.len().min(summary.credentials_received);
    summary.workers = workers.len();
    let mut sampled_processes = vec![(std::process::id(), "coordinator".to_owned())];
    sampled_processes.extend(drivers.iter().filter_map(|driver| {
        driver
            .child
            .id()
            .map(|pid| (pid, format!("driver-{}", driver.controller_id)))
    }));
    sampled_processes.extend(
        workers
            .iter()
            .enumerate()
            .filter_map(|(index, worker)| worker.id().map(|pid| (pid, format!("worker-{index}")))),
    );
    let sampler = spawn_process_sampler(
        result_directory.join("process-metrics.csv"),
        sampled_processes,
    );
    let (rotation_sender, mut rotation_receiver) = mpsc::channel(8192);
    let recorder = tokio::spawn(async move {
        while let Some((_, event)) = event_receiver.recv().await {
            if let Some(rotation) = record_driver_event(
                event,
                &mut BTreeMap::new(),
                &mut events,
                &mut summary,
                &mut ledger,
            )? {
                rotation_sender
                    .send(rotation)
                    .await
                    .context("credential rotation consumer stopped")?;
            }
        }
        Ok::<_, anyhow::Error>((summary, ledger))
    });

    tokio::time::sleep(arguments.ramp + Duration::from_millis(250)).await;
    record_failure_phase(&coordinator_events, FailurePhase::Stable).await?;
    wait_for_phase_control(arguments.phase_control.as_deref()).await?;
    let outcome = apply_scenario(&arguments, &plans, &mut drivers, &coordinator_events).await?;
    let replacement_credentials =
        collect_rotated_credentials(outcome.expected_rotations, &mut rotation_receiver).await?;
    if !replacement_credentials.is_empty() {
        let replacements = spawn_workers(
            &arguments,
            environment.mumble_server,
            &result_directory,
            replacement_credentials,
            workers.len(),
        )
        .await?;
        workers.extend(replacements);
    }
    if outcome.expected_rotations > 0 {
        record_failure_phase(&coordinator_events, FailurePhase::DegradedLoad).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        record_failure_phase(&coordinator_events, FailurePhase::Healing).await?;
        record_failure_phase(&coordinator_events, FailurePhase::Reconciliation).await?;
    }
    apply_process_fault(
        &arguments,
        &plans,
        &mut drivers,
        &mut workers,
        &known_credentials,
        &result_directory,
        &environment.controller_endpoint,
        environment.mumble_server,
        &coordinator_events,
        &mut rotation_receiver,
    )
    .await?;
    record_failure_phase(&coordinator_events, FailurePhase::FinalAudit).await?;
    for worker in &mut workers {
        let status = worker.wait().await.context("waiting for Mumble worker")?;
        ensure!(
            status.success()
                || is_resilience_scenario(arguments.scenario)
                || !matches!(arguments.fault, ProcessFault::None),
            "Mumble worker exited with {status}"
        );
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
    sampler.stop().await?;
    drop(coordinator_events);
    let (mut summary, ledger) = recorder.await.context("joining driver event recorder")??;
    audit_ledger(&plans, &ledger, arguments.scenario)?;
    summary.ownership_violations = ledger.credential_reuse_violations;
    summary.recovery_audits = 1;
    summary.elapsed_millis = duration_millis(started.elapsed());
    write_summary(&result_directory, &summary)?;
    mark_phase_control_complete(arguments.phase_control.as_deref())?;
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
    if !matches!(arguments.scenario, Scenario::Voice) {
        ensure!(
            arguments.audio.is_none(),
            "--audio only applies to the voice scenario"
        );
    }
    if effective_audio(arguments).is_some_and(|audio| !matches!(audio, AudioLevel::None)) {
        ensure!(
            arguments.voice_file.is_some(),
            "voice audio loads require --voice-file"
        );
    }
    if is_resilience_scenario(arguments.scenario) {
        ensure!(
            arguments.controllers >= 2,
            "resilience scenarios require at least two Controller IDs"
        );
    }
    if !matches!(arguments.fault, ProcessFault::None) {
        ensure!(
            arguments.controllers >= 2,
            "process faults require at least two Controller IDs"
        );
    }
    if !arguments.managed_bind_ip.is_loopback() {
        ensure!(
            matches!(arguments.mode, Mode::Managed),
            "--managed-bind-ip only applies to managed mode"
        );
    }
    for prefix in [
        arguments.driver_netns_prefix.as_deref(),
        arguments.worker_netns_prefix.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        ensure!(
            !prefix.is_empty(),
            "network namespace prefixes cannot be empty"
        );
        ensure!(
            cfg!(target_os = "linux"),
            "network namespace execution requires Linux"
        );
    }
    Ok(())
}

async fn start_environment(
    arguments: &RunArguments,
    result_directory: &Path,
) -> Result<ManagedEnvironment> {
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
    let connection_multiplier = if is_resilience_scenario(arguments.scenario)
        || !matches!(arguments.fault, ProcessFault::None)
    {
        2
    } else {
        1
    };
    let maximum_connections = u32::try_from(
        arguments
            .participants
            .max(1)
            .saturating_mul(connection_multiplier),
    )
    .context("managed connection limit exceeds u32")?;
    let config = ControllerConfig {
        controller_bind: SocketAddr::new(arguments.managed_bind_ip, 0),
        mumble_bind: SocketAddr::new(arguments.managed_bind_ip, 0),
        max_sessions: usize::from(arguments.controllers).max(1),
        max_participants: arguments.participants.max(1),
        max_participants_per_session: arguments.participants.max(1),
        max_spaces: arguments.participants.max(1),
        max_observations_per_session: arguments.participants.max(1),
        queue_capacity: arguments.participants.saturating_mul(2).max(1024),
        max_mumble_connections: maximum_connections,
        allow_unauthenticated_controller_network: !arguments.managed_bind_ip.is_loopback(),
        lease_duration: if is_resilience_scenario(arguments.scenario)
            || !matches!(arguments.fault, ProcessFault::None)
        {
            Duration::from_secs(3)
        } else {
            ControllerConfig::default().lease_duration
        },
        ..ControllerConfig::default()
    };
    let server = RunningControllerServer::start_with_metrics(
        config,
        identity,
        Some(MetricsOutput {
            path: result_directory.join("server-metrics.jsonl"),
            interval: Duration::from_secs(1),
        }),
    )
    .await?;
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
        drivers.push(spawn_driver(
            &arguments.driver,
            endpoint,
            index,
            result_directory,
            "",
            arguments.driver_netns_prefix.as_deref(),
            events.clone(),
        )?);
    }
    drop(events);
    Ok(drivers)
}

fn spawn_driver(
    executable: &Path,
    endpoint: &str,
    index: usize,
    result_directory: &Path,
    log_suffix: &str,
    netns_prefix: Option<&str>,
    events: mpsc::Sender<(usize, Value)>,
) -> Result<Driver> {
    let controller_id = format!("load-controller-{index}");
    let stderr = File::create(result_directory.join(format!("driver-{index}{log_suffix}.log")))?;
    let mut command = namespaced_command(netns_prefix, index, executable);
    let mut child = command
        .arg(endpoint)
        .arg(&controller_id)
        .arg("512")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting Java driver {}", executable.display()))?;
    let input = child.stdin.take().context("Java driver stdin")?;
    let output = child.stdout.take().context("Java driver stdout")?;
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
    Ok(Driver {
        controller_id,
        input,
        child,
        reader,
    })
}

fn namespaced_command(prefix: Option<&str>, index: usize, executable: &Path) -> ProcessCommand {
    if let Some(prefix) = prefix {
        let mut command = ProcessCommand::new("ip");
        command
            .arg("netns")
            .arg("exec")
            .arg(format!("{prefix}{index}"))
            .arg(executable);
        command
    } else {
        ProcessCommand::new(executable)
    }
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
) -> Result<Option<CredentialRotation>> {
    summary.driver_events = summary.driver_events.saturating_add(1);
    let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
    let mut rotation = None;
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
        ledger.record_credential(&participant, &credential);
        credentials.insert(participant.clone(), credential.clone());
        summary.credentials_received = summary.credentials_received.max(credentials.len());
        summary.credential_rotations = summary.credential_rotations.saturating_add(1);
        rotation = Some(CredentialRotation {
            participant_id: participant,
            credential,
        });
        event["kind"] = Value::String("credential_rotated".to_owned());
        if let Some(object) = event.as_object_mut() {
            object.remove("credential");
        }
    } else if kind == "error" {
        summary.errors = summary.errors.saturating_add(1);
    } else if kind == "ownership_lost" {
        summary.ownership_losses = summary.ownership_losses.saturating_add(1);
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
    Ok(rotation)
}

async fn spawn_workers(
    arguments: &RunArguments,
    mumble_server: SocketAddr,
    result_directory: &Path,
    credentials: BTreeMap<String, String>,
    worker_offset: usize,
) -> Result<Vec<Child>> {
    let executable = std::env::current_exe()?;
    let entries: Vec<_> = credentials.into_iter().collect();
    let mut workers = Vec::new();
    for (relative_index, chunk) in entries.chunks(MAX_CLIENTS_PER_WORKER).enumerate() {
        let index = worker_offset.saturating_add(relative_index);
        let stdout = File::create(result_directory.join(format!("worker-{index}.jsonl")))?;
        let stderr = File::create(result_directory.join(format!("worker-{index}.log")))?;
        let mut command =
            namespaced_command(arguments.worker_netns_prefix.as_deref(), index, &executable);
        command
            .arg("worker")
            .arg("--mumble-server")
            .arg(mumble_server.to_string())
            .arg("--duration")
            .arg(format!("{}ms", arguments.duration.as_millis()))
            .arg("--ramp")
            .arg(format!("{}ms", arguments.ramp.as_millis()))
            .arg("--talk-percent")
            .arg("100")
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
                voice_enabled: speaker_enabled(arguments, participant_id),
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
            voice_enabled: credential.voice_enabled,
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
    events: &mpsc::Sender<(usize, Value)>,
) -> Result<ScenarioOutcome> {
    let mut outcome = ScenarioOutcome::default();
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
        Scenario::BatchTakeover => {
            record_failure_phase(events, FailurePhase::Injection).await?;
            for plan in plans {
                let target = (plan.controller + 1) % drivers.len();
                register_on(drivers, plan, target, "batch-takeover").await?;
                record_expected_owner(events, plan, target).await?;
                outcome.expected_rotations = outcome.expected_rotations.saturating_add(1);
            }
        }
        Scenario::SimultaneousClaim => {
            record_failure_phase(events, FailurePhase::Injection).await?;
            let plan = plans.first().context("simultaneous claim participant")?;
            let target = (plan.controller + 1) % drivers.len();
            register_on(drivers, plan, target, "simultaneous-claim").await?;
            record_expected_owner(events, plan, target).await?;
            outcome.expected_rotations = 1;
        }
        Scenario::CrossedTakeover => {
            record_failure_phase(events, FailurePhase::Injection).await?;
            for plan in plans.iter().take(2) {
                let target = (plan.controller + 1) % drivers.len();
                register_on(drivers, plan, target, "crossed-takeover").await?;
                record_expected_owner(events, plan, target).await?;
                outcome.expected_rotations = outcome.expected_rotations.saturating_add(1);
            }
        }
        Scenario::PingPong => {
            record_failure_phase(events, FailurePhase::Injection).await?;
            let plan = plans.first().context("ping-pong participant")?;
            let first_target = (plan.controller + 1) % drivers.len();
            register_on(drivers, plan, first_target, "ping-pong-1").await?;
            record_expected_owner(events, plan, first_target).await?;
            outcome.expected_rotations = 1;
        }
        Scenario::StaleReconnect | Scenario::IdentityBoundary | Scenario::OverloadRecovery => {
            // Their named fault is injected below, after the stable load is established.
        }
        _ => {}
    }
    Ok(outcome)
}

async fn register_on(
    drivers: &mut [Driver],
    plan: &ParticipantPlan,
    target: usize,
    correlation: &str,
) -> Result<()> {
    send_driver_command(
        drivers.get_mut(target).context("takeover driver")?,
        json!({
            "schema_version": PROTOCOL_VERSION,
            "kind": "register",
            "correlation_id": format!("{correlation}-{}", plan.participant_id),
            "participant_id": plan.participant_id,
            "space_key": plan.space_key,
            "display_name": plan.display_name,
            "server_mute": false,
            "server_deaf": false,
        }),
    )
    .await
}

async fn record_expected_owner(
    events: &mpsc::Sender<(usize, Value)>,
    plan: &ParticipantPlan,
    controller: usize,
) -> Result<()> {
    events
        .send((
            controller,
            json!({
                "schema_version": PROTOCOL_VERSION,
                "kind": "expected_owner",
                "correlation_id": "coordinator-ledger",
                "controller_id": format!("load-controller-{controller}"),
                "participant_id": plan.participant_id,
                "coordinator_unix_nanos": unix_nanos().to_string(),
            }),
        ))
        .await
        .context("event recorder stopped")
}

async fn record_failure_phase(
    events: &mpsc::Sender<(usize, Value)>,
    phase: FailurePhase,
) -> Result<()> {
    events
        .send((
            usize::MAX,
            json!({
                "schema_version": PROTOCOL_VERSION,
                "kind": "failure_phase",
                "phase": phase,
                "correlation_id": "failure-phase",
                "controller_id": "coordinator",
                "participant_id": "",
                "coordinator_unix_nanos": unix_nanos().to_string(),
            }),
        ))
        .await
        .context("event recorder stopped")
}

async fn collect_rotated_credentials(
    expected: usize,
    rotations: &mut mpsc::Receiver<CredentialRotation>,
) -> Result<BTreeMap<String, String>> {
    if expected == 0 {
        return Ok(BTreeMap::new());
    }
    timeout(OPERATION_DEADLINE, async {
        let mut received = 0usize;
        let mut credentials = BTreeMap::new();
        while received < expected {
            let rotation = rotations
                .recv()
                .await
                .context("credential rotation recorder stopped")?;
            credentials.insert(rotation.participant_id, rotation.credential);
            received = received.saturating_add(1);
        }
        Ok::<_, anyhow::Error>(credentials)
    })
    .await
    .context("credential rotation timeout")?
}

#[allow(clippy::too_many_arguments)]
async fn apply_process_fault(
    arguments: &RunArguments,
    plans: &[ParticipantPlan],
    drivers: &mut [Driver],
    workers: &mut Vec<Child>,
    known_credentials: &BTreeMap<String, String>,
    result_directory: &Path,
    controller_endpoint: &str,
    mumble_server: SocketAddr,
    events: &mpsc::Sender<(usize, Value)>,
    rotations: &mut mpsc::Receiver<CredentialRotation>,
) -> Result<()> {
    let fault = effective_fault(arguments);
    if matches!(fault, ProcessFault::None) {
        return Ok(());
    }
    record_failure_phase(events, FailurePhase::Injection).await?;
    let mut expected_rotations = 0usize;
    let mut restart_workers = false;
    match fault {
        ProcessFault::None => {}
        ProcessFault::GracefulStop | ProcessFault::Crash => {
            terminate_workers(workers).await?;
            restart_workers = true;
            restart_driver(
                arguments,
                plans,
                drivers,
                0,
                result_directory,
                controller_endpoint,
                events,
                matches!(fault, ProcessFault::GracefulStop),
            )
            .await?;
            expected_rotations = plans.iter().filter(|plan| plan.controller == 0).count();
        }
        ProcessFault::FreezeShort | ProcessFault::FreezeLong => {
            let duration = if matches!(fault, ProcessFault::FreezeLong) {
                arguments.fault_duration.max(Duration::from_secs(4))
            } else {
                arguments.fault_duration.min(Duration::from_secs(1))
            };
            signal_driver(drivers.first().context("fault target driver")?, "STOP").await?;
            record_failure_phase(events, FailurePhase::DegradedLoad).await?;
            tokio::time::sleep(duration).await;
            record_failure_phase(events, FailurePhase::Healing).await?;
            signal_driver(drivers.first().context("fault target driver")?, "CONT").await?;
            if matches!(fault, ProcessFault::FreezeLong) {
                terminate_workers(workers).await?;
                restart_workers = true;
                expected_rotations = plans.iter().filter(|plan| plan.controller == 0).count();
            }
        }
        ProcessFault::HostRestart => {
            terminate_workers(workers).await?;
            restart_workers = true;
            for index in 0..drivers.len() {
                restart_driver(
                    arguments,
                    plans,
                    drivers,
                    index,
                    result_directory,
                    controller_endpoint,
                    events,
                    false,
                )
                .await?;
            }
            expected_rotations = plans.len();
        }
        ProcessFault::MumbleCut => {
            terminate_workers(workers).await?;
            restart_workers = true;
        }
        ProcessFault::ControlledLimit => {
            record_failure_phase(events, FailurePhase::DegradedLoad).await?;
            let operations = 1_024usize.max(plans.len().saturating_mul(4));
            for index in 0..operations {
                let plan = &plans[index % plans.len()];
                send_spec(
                    drivers,
                    plan,
                    plan.space_key.clone(),
                    index.is_multiple_of(2),
                    false,
                )
                .await?;
            }
            tokio::time::sleep(arguments.fault_duration).await;
            record_failure_phase(events, FailurePhase::Healing).await?;
            for index in 0..operations.saturating_mul(4) / 5 {
                let plan = &plans[index % plans.len()];
                send_spec(drivers, plan, plan.space_key.clone(), false, false).await?;
            }
        }
    }
    if !matches!(
        fault,
        ProcessFault::FreezeShort | ProcessFault::ControlledLimit
    ) {
        record_failure_phase(events, FailurePhase::DegradedLoad).await?;
        tokio::time::sleep(Duration::from_millis(250)).await;
        record_failure_phase(events, FailurePhase::Healing).await?;
    }
    if restart_workers {
        let mut credentials = known_credentials.clone();
        credentials.extend(collect_rotated_credentials(expected_rotations, rotations).await?);
        let replacements = spawn_workers(
            arguments,
            mumble_server,
            result_directory,
            credentials,
            workers.len().saturating_add(1000),
        )
        .await?;
        workers.extend(replacements);
    }
    record_failure_phase(events, FailurePhase::Reconciliation).await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

fn effective_fault(arguments: &RunArguments) -> ProcessFault {
    if !matches!(arguments.fault, ProcessFault::None) {
        return arguments.fault;
    }
    match arguments.scenario {
        Scenario::StaleReconnect => ProcessFault::FreezeLong,
        Scenario::IdentityBoundary => ProcessFault::Crash,
        Scenario::OverloadRecovery => ProcessFault::ControlledLimit,
        _ => ProcessFault::None,
    }
}

fn effective_audio(arguments: &RunArguments) -> Option<AudioLevel> {
    if matches!(arguments.scenario, Scenario::Voice) {
        Some(arguments.audio.unwrap_or(AudioLevel::FivePercent))
    } else {
        None
    }
}

fn speaker_enabled(arguments: &RunArguments, participant_id: &str) -> bool {
    speaker_enabled_for(
        effective_audio(arguments),
        arguments.participants_per_space,
        participant_id,
    )
}

fn speaker_enabled_for(
    audio: Option<AudioLevel>,
    participants_per_space: usize,
    participant_id: &str,
) -> bool {
    let Some(audio) = audio else {
        return false;
    };
    let Some(index) = participant_id
        .strip_prefix("load-participant-")
        .and_then(|value| value.parse::<usize>().ok())
    else {
        return false;
    };
    match audio {
        AudioLevel::None => false,
        AudioLevel::OnePerSpace => index.is_multiple_of(participants_per_space),
        AudioLevel::FivePercent => index.is_multiple_of(20),
    }
}

async fn terminate_workers(workers: &mut Vec<Child>) -> Result<()> {
    for worker in &mut *workers {
        worker.kill().await.context("cutting Mumble worker")?;
    }
    workers.clear();
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn restart_driver(
    arguments: &RunArguments,
    plans: &[ParticipantPlan],
    drivers: &mut [Driver],
    index: usize,
    result_directory: &Path,
    endpoint: &str,
    events: &mpsc::Sender<(usize, Value)>,
    graceful: bool,
) -> Result<()> {
    let driver = drivers.get_mut(index).context("fault target driver")?;
    if graceful {
        send_driver_command(
            driver,
            json!({
                "schema_version": PROTOCOL_VERSION,
                "kind": "shutdown",
                "correlation_id": format!("fault-shutdown-{index}"),
            }),
        )
        .await?;
        timeout(OPERATION_DEADLINE, driver.child.wait())
            .await
            .context("graceful driver stop timeout")??;
    } else {
        driver.child.kill().await.context("crashing Java driver")?;
    }
    (&mut driver.reader)
        .await
        .context("joining stopped Java driver reader")?;
    let replacement = spawn_driver(
        &arguments.driver,
        endpoint,
        index,
        result_directory,
        "-restarted",
        arguments.driver_netns_prefix.as_deref(),
        events.clone(),
    )?;
    drivers[index] = replacement;
    tokio::time::sleep(Duration::from_millis(250)).await;
    for plan in plans.iter().filter(|plan| plan.controller == index) {
        register_on(drivers, plan, index, "recovery-register").await?;
    }
    Ok(())
}

#[cfg(unix)]
async fn signal_driver(driver: &Driver, signal: &str) -> Result<()> {
    let pid = driver.child.id().context("Java driver has no pid")?;
    let status = ProcessCommand::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .await?;
    ensure!(
        status.success(),
        "kill -{signal} {pid} exited with {status}"
    );
    Ok(())
}

#[cfg(not(unix))]
async fn signal_driver(_driver: &Driver, _signal: &str) -> Result<()> {
    anyhow::bail!("freeze faults require a Unix host")
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
            let shuffled = index.wrapping_add(usize::try_from(seed).unwrap_or(0));
            let controller = if matches!(scenario, Scenario::Skew90_10) {
                let primary = participants.saturating_mul(9) / 10;
                if index < primary {
                    0
                } else {
                    1 + shuffled % controllers.saturating_sub(1).max(1)
                }
            } else {
                shuffled % controllers
            };
            let space = match scenario {
                Scenario::OneSpace | Scenario::MultiSharedSpaces => 0,
                Scenario::ManySpaces => index,
                Scenario::MultiIsolatedSpaces => controller,
                _ => index / participants_per_space,
            };
            ParticipantPlan {
                participant_id: format!("load-participant-{index}"),
                display_name: format!("Load participant {index}"),
                space_key: format!("load-space-{space}"),
                controller,
            }
        })
        .collect()
}

fn is_resilience_scenario(scenario: Scenario) -> bool {
    matches!(
        scenario,
        Scenario::MultiSharedSpaces
            | Scenario::MultiIsolatedSpaces
            | Scenario::Skew90_10
            | Scenario::BatchTakeover
            | Scenario::SimultaneousClaim
            | Scenario::CrossedTakeover
            | Scenario::PingPong
            | Scenario::StaleReconnect
            | Scenario::IdentityBoundary
            | Scenario::OverloadRecovery
    )
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

async fn wait_for_phase_control(directory: Option<&Path>) -> Result<()> {
    let Some(directory) = directory else {
        return Ok(());
    };
    std::fs::create_dir_all(directory)?;
    std::fs::write(directory.join("stable.ready"), b"stable\n")?;
    timeout(DRIVER_START_DEADLINE, async {
        loop {
            if directory.join("injection.go").is_file() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("phase-control injection timeout")?;
    Ok(())
}

fn mark_phase_control_complete(directory: Option<&Path>) -> Result<()> {
    if let Some(directory) = directory {
        std::fs::write(directory.join("complete.ready"), b"complete\n")?;
    }
    Ok(())
}

fn write_load_matrix(path: &Path) -> Result<()> {
    let cardinalities = [8usize, 32, 64, 128, 256];
    let requested_totals = [25usize, 50, 100, 200, 400, 800, 1_600, 3_200, 6_400, 10_000];
    let audio_levels = [
        AudioLevel::None,
        AudioLevel::OnePerSpace,
        AudioLevel::FivePercent,
    ];
    let mut points = Vec::new();
    for participants_per_space in cardinalities {
        let mut totals = BTreeSet::from([participants_per_space, 2_048, 4_096]);
        for requested in requested_totals {
            if requested > participants_per_space {
                totals.insert(requested / participants_per_space * participants_per_space);
            }
        }
        for participants in totals {
            for audio in audio_levels {
                points.push(LoadPoint {
                    participants_per_space,
                    participants,
                    spaces: participants / participants_per_space,
                    audio,
                });
            }
        }
    }
    let matrix = LoadMatrix {
        schema_version: PROTOCOL_VERSION,
        points,
        fixed_totals: [2_048, 4_096],
        resilience_capacity_percent: [50, 70, 90],
        full_fault_cardinalities: [32, 128],
        edge_fault_cardinalities: [8, 256],
        healthy_thresholds: HealthyThresholds {
            maximum_server_cpu_percent: 85,
            maximum_generator_cpu_percent: 60,
            synchronization_p99_millis: 2_000,
            migration_p99_millis: 500,
            mute_p99_millis: 500,
            ping_p99_millis: 100,
            minimum_audio_delivery_percent: 99.9,
        },
    };
    serde_json::to_writer_pretty(File::create(path)?, &matrix)?;
    Ok(())
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
        audio: effective_audio(arguments),
        fault: arguments.fault,
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
        "scenario,controllers,participants,credentials,workers,driver_events,credential_rotations,ownership_losses,ownership_violations,stale_credential_rejections,recovery_audits,errors,elapsed_millis"
    )?;
    writeln!(
        csv,
        "{},{},{},{},{},{},{},{},{},{},{},{},{}",
        summary.scenario,
        summary.controllers,
        summary.participants_requested,
        summary.credentials_received,
        summary.workers,
        summary.driver_events,
        summary.credential_rotations,
        summary.ownership_losses,
        summary.ownership_violations,
        summary.stale_credential_rejections,
        summary.recovery_audits,
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

fn spawn_process_sampler(path: PathBuf, processes: Vec<(u32, String)>) -> ProcessSampler {
    let (stop, mut stopped) = oneshot::channel();
    let task = tokio::spawn(async move {
        let mut file = tokio::fs::File::create(path).await?;
        file.write_all(b"unix_millis,pid,role,rss_kib,cpu_percent\n")
            .await?;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                // Cancellation-safe: the next interval deadline remains owned by the interval.
                _ = interval.tick() => sample_processes(&mut file, &processes).await?,
                // Cancellation-safe: the one-shot is consumed only when shutdown is requested.
                _ = &mut stopped => {
                    file.flush().await?;
                    return Ok(());
                }
            }
        }
    });
    ProcessSampler { stop, task }
}

#[cfg(unix)]
async fn sample_processes(file: &mut tokio::fs::File, processes: &[(u32, String)]) -> Result<()> {
    let timestamp = unix_nanos() / 1_000_000;
    for (pid, role) in processes {
        let pid_string = pid.to_string();
        let output = ProcessCommand::new("ps")
            .args(["-o", "rss=", "-o", "%cpu=", "-p", &pid_string])
            .output()
            .await?;
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8(output.stdout)?;
        let mut values = text.split_whitespace();
        let Some(rss) = values.next() else {
            continue;
        };
        let Some(cpu) = values.next() else {
            continue;
        };
        file.write_all(format!("{timestamp},{pid},{role},{rss},{cpu}\n").as_bytes())
            .await?;
    }
    file.flush().await?;
    Ok(())
}

#[cfg(not(unix))]
async fn sample_processes(file: &mut tokio::fs::File, processes: &[(u32, String)]) -> Result<()> {
    let timestamp = unix_nanos() / 1_000_000;
    for (pid, role) in processes {
        file.write_all(format!("{timestamp},{pid},{role},0,0\n").as_bytes())
            .await?;
    }
    file.flush().await?;
    Ok(())
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
                    current_owner: String::new(),
                    previous_owner: String::new(),
                    ownership_losses: 0,
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
            "owned" => {
                if participant.current_owner != controller {
                    participant.previous_owner = participant.current_owner.clone();
                    participant.current_owner = controller.to_owned();
                }
                participant.owned = true;
            }
            "ownership_lost" => {
                participant.ownership_losses = participant.ownership_losses.saturating_add(1);
                if participant.current_owner == controller {
                    participant.previous_owner = participant.current_owner.clone();
                }
            }
            "expected_owner" => {
                participant.expected_controller = controller.to_owned();
            }
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

    fn record_credential(&mut self, _participant_id: &str, credential: &str) {
        if !self.seen_credentials.insert(credential.to_owned()) {
            self.credential_reuse_violations = self.credential_reuse_violations.saturating_add(1);
        }
    }
}

fn audit_ledger(plans: &[ParticipantPlan], ledger: &Ledger, scenario: Scenario) -> Result<()> {
    ensure!(
        ledger.participants.len() == plans.len(),
        "ledger participant count diverged"
    );
    for plan in plans {
        let participant = ledger
            .participants
            .get(&plan.participant_id)
            .context("participant missing from ledger")?;
        if !is_resilience_scenario(scenario) {
            ensure!(
                participant.expected_controller == format!("load-controller-{}", plan.controller),
                "ledger controller assignment diverged for {}",
                plan.participant_id
            );
        }
        ensure!(participant.owned, "{} was never owned", plan.participant_id);
        if !matches!(scenario, Scenario::Churn) {
            ensure!(
                participant.current_owner == participant.expected_controller,
                "{} expected owner {}, observed {}",
                plan.participant_id,
                participant.expected_controller,
                participant.current_owner
            );
        }
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
    ensure!(
        ledger.credential_reuse_violations == 0,
        "a connection credential was reused"
    );
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
            voice_enabled: bool,
        }
        Wire {
            participant_id: &self.participant_id,
            credential: &self.credential,
            voice_enabled: self.voice_enabled,
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

    #[test]
    fn load_matrix_keeps_spaces_full_for_every_cardinality() -> Result<()> {
        let temporary = std::env::temp_dir().join(format!(
            "mumble-spaces-load-matrix-test-{}.json",
            std::process::id()
        ));
        write_load_matrix(&temporary)?;
        let matrix: Value = serde_json::from_reader(File::open(&temporary)?)?;
        std::fs::remove_file(&temporary)?;
        let points = matrix["points"].as_array().context("matrix points")?;
        assert!(points.iter().all(|point| {
            let participants = point["participants"].as_u64().unwrap_or(0);
            let per_space = point["participants_per_space"].as_u64().unwrap_or(1);
            participants.is_multiple_of(per_space)
        }));
        for cardinality in [8u64, 32, 64, 128, 256] {
            for fixed in [2_048u64, 4_096] {
                assert!(points.iter().any(|point| {
                    point["participants_per_space"] == cardinality && point["participants"] == fixed
                }));
            }
        }
        Ok(())
    }

    #[test]
    fn voice_speakers_match_the_documented_audio_loads() {
        let one_per_space = (0..64)
            .filter(|index| {
                speaker_enabled_for(
                    Some(AudioLevel::OnePerSpace),
                    8,
                    &format!("load-participant-{index}"),
                )
            })
            .count();
        assert_eq!(one_per_space, 8);

        let five_percent = (0..100)
            .filter(|index| {
                speaker_enabled_for(
                    Some(AudioLevel::FivePercent),
                    8,
                    &format!("load-participant-{index}"),
                )
            })
            .count();
        assert_eq!(five_percent, 5);
        assert!(!speaker_enabled_for(
            Some(AudioLevel::FivePercent),
            8,
            "unexpected-participant"
        ));
    }

    #[test]
    fn skew_plan_is_deterministic_and_approximately_ninety_ten() {
        let plans = participant_plans(100, 8, 2, Scenario::Skew90_10, 9);
        assert_eq!(plans.iter().filter(|plan| plan.controller == 0).count(), 90);
        assert_eq!(plans.iter().filter(|plan| plan.controller == 1).count(), 10);
    }

    #[test]
    fn failure_phases_are_stable_and_ordered() -> Result<()> {
        let phases = [
            FailurePhase::Stable,
            FailurePhase::Injection,
            FailurePhase::DegradedLoad,
            FailurePhase::Healing,
            FailurePhase::Reconciliation,
            FailurePhase::FinalAudit,
        ];
        assert_eq!(
            serde_json::to_string(&phases)?,
            "[\"stable\",\"injection\",\"degraded_load\",\"healing\",\"reconciliation\",\"final_audit\"]"
        );
        Ok(())
    }

    #[tokio::test]
    async fn phase_control_waits_for_explicit_injection_release() -> Result<()> {
        let directory = std::env::temp_dir().join(format!(
            "mumble-spaces-phase-control-test-{}",
            std::process::id()
        ));
        let controlled_directory = directory.clone();
        let waiter =
            tokio::spawn(async move { wait_for_phase_control(Some(&controlled_directory)).await });
        timeout(Duration::from_secs(2), async {
            while !directory.join("stable.ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        assert!(!waiter.is_finished());
        std::fs::write(directory.join("injection.go"), b"go\n")?;
        waiter.await??;
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }
}
