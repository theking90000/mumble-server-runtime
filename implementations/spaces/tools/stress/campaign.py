#!/usr/bin/env python3
"""Run reproducible, resumable Spaces headless load campaigns."""

from __future__ import annotations

import argparse
import base64
import json
import math
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


SCHEMA_VERSION = 1
DRIVER_OPERATION_WINDOW = 512
MAX_CONTROLLERS = 64
LARGE_LOAD_THRESHOLD = 4_096
ALLOWED_CARDINALITIES = {8, 32, 64, 128, 256}
AUDIO_LEVELS = ("none", "one_per_space", "five_percent")
ALLOWED_AUDIO_LEVELS = set(AUDIO_LEVELS)
DURATION_PATTERN = re.compile(r"^[0-9]+(?:ms|s|m)$")
SCRIPT_DIRECTORY = Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_DIRECTORY.parents[3]
BENCHMARK_TARGET_DIRECTORY = REPOSITORY_ROOT / "target/spaces-load-metrics"
DEFAULT_BINARY = BENCHMARK_TARGET_DIRECTORY / "release/mumble-spaces-stress"
DEFAULT_DRIVER = (
    REPOSITORY_ROOT
    / "implementations/spaces/tools/load-driver-java/build/install/load-driver-java/bin/load-driver-java"
)
DEFAULT_FIXTURE = SCRIPT_DIRECTORY / "fixtures/smoke.opuspack.base64"
REPORT_SCRIPT = SCRIPT_DIRECTORY / "report.py"


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat()


def parse_csv_integers(value: str) -> list[int]:
    try:
        parsed = [int(part) for part in value.split(",") if part]
    except ValueError as error:
        raise argparse.ArgumentTypeError(f"expected comma-separated integers: {error}") from error
    if not parsed or any(number < 0 for number in parsed):
        raise argparse.ArgumentTypeError("expected one or more non-negative integers")
    return parsed


def parse_cardinalities(value: str) -> list[int]:
    cardinalities = parse_csv_integers(value)
    unexpected = set(cardinalities) - ALLOWED_CARDINALITIES
    if unexpected:
        raise argparse.ArgumentTypeError(
            f"unsupported Space cardinalities: {', '.join(map(str, sorted(unexpected)))}"
        )
    return sorted(set(cardinalities))


def parse_duration(value: str) -> str:
    if not DURATION_PATTERN.fullmatch(value):
        raise argparse.ArgumentTypeError("use a duration suffix: ms, s, or m")
    return value


def parse_controllers(value: str) -> str | int:
    if value == "auto":
        return value
    try:
        controllers = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("controllers must be auto or an integer") from error
    if not 1 <= controllers <= MAX_CONTROLLERS:
        raise argparse.ArgumentTypeError(f"controllers must be between 1 and {MAX_CONTROLLERS}")
    return controllers


def next_power_of_two(value: int) -> int:
    return 1 if value <= 1 else 1 << (value - 1).bit_length()


def controller_count(participants: int, requested: str | int) -> int:
    minimum = math.ceil(participants / DRIVER_OPERATION_WINDOW)
    if requested == "auto":
        controllers = next_power_of_two(minimum) if minimum <= 8 else minimum
    else:
        controllers = requested
    if controllers > MAX_CONTROLLERS:
        raise ValueError(
            f"{participants} participants require more than {MAX_CONTROLLERS} Controllers"
        )
    if math.ceil(participants / controllers) > DRIVER_OPERATION_WINDOW:
        raise ValueError(
            f"{participants} participants exceed the Java operation window with "
            f"{controllers} Controller(s); use at least {minimum}"
        )
    return controllers


def point_identifier(point: dict[str, Any]) -> str:
    audio = str(point["audio"]).replace("_", "-")
    return (
        f"k{int(point['participants_per_space']):03d}-"
        f"n{int(point['participants']):05d}-{audio}-s{int(point['seed'])}"
    )


def plan_points(
    matrix: dict[str, Any],
    profile: str,
    cardinalities: Iterable[int],
    max_participants: int,
    seeds: Iterable[int],
    controllers: str | int,
) -> list[dict[str, Any]]:
    matrix_points = matrix.get("points")
    if not isinstance(matrix_points, list):
        raise ValueError("matrix does not contain a points array")
    selected_cardinalities = set(cardinalities)
    base_points = []
    for raw in matrix_points:
        if not isinstance(raw, dict):
            raise ValueError("matrix point is not an object")
        per_space = int(raw.get("participants_per_space", 0))
        participants = int(raw.get("participants", 0))
        audio = str(raw.get("audio", ""))
        if per_space not in selected_cardinalities or participants > max_participants:
            continue
        if audio not in ALLOWED_AUDIO_LEVELS:
            raise ValueError(f"unsupported matrix audio level: {audio}")
        if profile == "quick" and not (participants == per_space and audio == "none"):
            continue
        if profile == "capacity" and audio != "none":
            continue
        base_points.append(
            {
                "participants_per_space": per_space,
                "participants": participants,
                "spaces": int(raw.get("spaces", participants // per_space)),
                "audio": audio,
            }
        )
    points = []
    for base in sorted(
        base_points,
        key=lambda item: (
            item["participants_per_space"],
            item["participants"],
            AUDIO_LEVELS.index(item["audio"]),
        ),
    ):
        for seed in sorted(set(seeds)):
            point = {
                **base,
                "seed": seed,
                "controllers": controller_count(base["participants"], controllers),
                "status": "pending",
                "attempts": 0,
            }
            point["id"] = point_identifier(point)
            points.append(point)
    if not points:
        raise ValueError("the selected profile and filters produce no benchmark point")
    return points


def load_matrix(binary: Path, matrix_file: Path | None) -> dict[str, Any]:
    if matrix_file is not None:
        with matrix_file.open(encoding="utf-8") as source:
            value = json.load(source)
    else:
        if not binary.is_file():
            raise ValueError(f"stress binary not found: {binary}; build it or pass --matrix-file")
        with tempfile.TemporaryDirectory(prefix="spaces-load-matrix-") as temporary:
            output = Path(temporary) / "matrix.json"
            subprocess.run(
                [str(binary), "matrix", "--output", str(output)],
                cwd=REPOSITORY_ROOT,
                check=True,
            )
            with output.open(encoding="utf-8") as source:
                value = json.load(source)
    if not isinstance(value, dict) or int(value.get("schema_version", 0)) != SCHEMA_VERSION:
        raise ValueError("unsupported or missing load matrix schema_version")
    return value


def command_for_point(
    point: dict[str, Any],
    binary: Path,
    driver: Path,
    run_root: Path,
    voice_file: Path | None,
    duration: str,
    ramp: str,
) -> list[str]:
    audio = str(point["audio"])
    scenario = "idle" if audio == "none" else "voice"
    command = [
        str(binary),
        "run",
        "--mode",
        "managed",
        "--driver",
        str(driver),
        "--controllers",
        str(point["controllers"]),
        "--participants",
        str(point["participants"]),
        "--participants-per-space",
        str(point["participants_per_space"]),
        "--scenario",
        scenario,
        "--seed",
        str(point["seed"]),
        "--ramp",
        ramp,
        "--duration",
        duration,
        "--result-root",
        str(run_root),
    ]
    if audio != "none":
        if voice_file is None:
            raise ValueError("audio benchmark point has no prepared voice file")
        command.extend(
            [
                "--audio",
                audio.replace("_", "-"),
                "--voice-file",
                str(voice_file),
            ]
        )
    return command


def required_open_files(points: list[dict[str, Any]]) -> int:
    maximum = 0
    for point in points:
        participants = int(point["participants"])
        controllers = int(point["controllers"])
        workers = math.ceil(participants / DRIVER_OPERATION_WINDOW)
        coordinator = participants + 1_024 + 4 * (controllers + workers)
        worker = 2 * min(participants, DRIVER_OPERATION_WINDOW) + 256
        maximum = max(maximum, coordinator, worker)
    return maximum


def open_file_limit() -> int | None:
    try:
        import resource
    except ImportError:
        return None
    soft, _ = resource.getrlimit(resource.RLIMIT_NOFILE)
    return int(soft)


def preflight(
    points: list[dict[str, Any]],
    binary: Path,
    driver: Path,
    skip_nofile_check: bool,
) -> None:
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError(f"release stress binary is missing or not executable: {binary}")
    if not driver.is_file() or not os.access(driver, os.X_OK):
        raise ValueError(f"Java load driver is missing or not executable: {driver}")
    if skip_nofile_check:
        return
    available = open_file_limit()
    required = required_open_files(points)
    if available is not None and available < required:
        raise ValueError(
            f"open-file soft limit {available} is below the estimated requirement {required}; "
            f"run 'ulimit -n {max(65_536, required)}' in this shell or use "
            "--skip-nofile-check after reviewing the risk"
        )


def atomic_json(path: Path, value: dict[str, Any]) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    os.replace(temporary, path)


def emit_event(campaign: Path, kind: str, **fields: Any) -> None:
    event = {"schema_version": SCHEMA_VERSION, "timestamp": utc_now(), "kind": kind, **fields}
    with (campaign / "runner-events.jsonl").open("a", encoding="utf-8") as output:
        output.write(json.dumps(event, separators=(",", ":")) + "\n")


def run_logged(command: list[str], log_path: Path) -> int:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    process = subprocess.Popen(
        command,
        cwd=REPOSITORY_ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
        start_new_session=True,
    )
    try:
        with log_path.open("a", encoding="utf-8") as log:
            if process.stdout is not None:
                for line in process.stdout:
                    sys.stdout.write(line)
                    log.write(line)
                    log.flush()
        return process.wait()
    except KeyboardInterrupt:
        terminate_process(process)
        raise


def terminate_process(process: subprocess.Popen[str]) -> None:
    if process.poll() is not None:
        return
    if os.name == "posix":
        os.killpg(process.pid, signal.SIGTERM)
    else:
        process.terminate()
    try:
        process.wait(timeout=5)
    except subprocess.TimeoutExpired:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGKILL)
        else:
            process.kill()
        process.wait()


def build_components(campaign: Path) -> None:
    build_log = campaign / "build.log"
    commands = [
        [
            str(REPOSITORY_ROOT / "gradlew"),
            ":implementations:spaces:tools:load-driver-java:installDist",
        ],
        [
            "cargo",
            "build",
            "--locked",
            "--release",
            "-p",
            "mumble-spaces-stress",
            "--features",
            "load-metrics",
        ],
    ]
    environment = os.environ.copy()
    environment.setdefault("GRADLE_USER_HOME", str(REPOSITORY_ROOT / ".gradle"))
    environment["CARGO_TARGET_DIR"] = str(BENCHMARK_TARGET_DIRECTORY)
    for command in commands:
        with build_log.open("a", encoding="utf-8") as log:
            log.write(f"$ {' '.join(command)}\n")
        process = subprocess.Popen(
            command,
            cwd=REPOSITORY_ROOT,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            start_new_session=True,
        )
        try:
            with build_log.open("a", encoding="utf-8") as log:
                if process.stdout is not None:
                    for line in process.stdout:
                        sys.stdout.write(line)
                        log.write(line)
            return_code = process.wait()
        except KeyboardInterrupt:
            terminate_process(process)
            raise
        if return_code:
            raise RuntimeError(f"build command exited with {return_code}: {' '.join(command)}")


def prepare_voice_file(campaign: Path, fixture: Path) -> Path:
    try:
        encoded = b"".join(fixture.read_bytes().split())
        decoded = base64.b64decode(encoded, validate=True)
    except (OSError, ValueError) as error:
        raise ValueError(f"could not decode voice fixture {fixture}: {error}") from error
    if not decoded or len(decoded) % 30:
        raise ValueError("voice fixture is empty or not made of 30-byte Opus packets")
    destination = campaign / "voice.opuspack"
    destination.write_bytes(decoded)
    return destination


def run_directories(root: Path) -> set[Path]:
    if not root.is_dir():
        return set()
    return {path.resolve() for path in root.iterdir() if path.is_dir()}


def discover_run_directory(before: set[Path], run_root: Path) -> Path | None:
    created = run_directories(run_root) - before
    if not created:
        return None
    return max(created, key=lambda path: path.stat().st_mtime_ns)


def generate_reports(campaign: Path, run_root: Path, run_directory: Path | None = None) -> None:
    if run_directory is not None:
        subprocess.run(
            [sys.executable, str(REPORT_SCRIPT), str(run_directory)],
            cwd=REPOSITORY_ROOT,
            check=False,
        )
    if any((path / "manifest.json").is_file() for path in run_root.iterdir() if path.is_dir()):
        subprocess.run(
            [
                sys.executable,
                str(REPORT_SCRIPT),
                str(run_root),
                "--output",
                str(campaign / "index.html"),
            ],
            cwd=REPOSITORY_ROOT,
            check=False,
        )


def print_plan(points: list[dict[str, Any]]) -> None:
    print("id                                           ctrl  participants  K    spaces  audio")
    for point in points:
        print(
            f"{point['id']:<44} {point['controllers']:>4}  {point['participants']:>12}  "
            f"{point['participants_per_space']:>3}  {point['spaces']:>6}  {point['audio']}"
        )
    print(f"\n{len(points)} benchmark run(s), estimated RLIMIT_NOFILE: {required_open_files(points)}")


def campaign_configuration(arguments: argparse.Namespace) -> dict[str, Any]:
    return {
        "profile": arguments.profile,
        "cardinalities": arguments.cardinalities,
        "max_participants": arguments.max_participants,
        "seeds": arguments.seeds,
        "controllers": arguments.controllers,
        "duration": arguments.duration,
        "ramp": arguments.ramp,
        "cooldown_seconds": arguments.cooldown,
        "binary": str(arguments.binary.resolve()),
        "driver": str(arguments.driver.resolve()),
        "voice_fixture": str(arguments.voice_fixture.resolve()),
        "skip_build": arguments.skip_build,
        "skip_nofile_check": arguments.skip_nofile_check,
    }


def allocate_campaign(result_root: Path) -> Path:
    identifier = datetime.now(timezone.utc).strftime("campaign-%Y%m%dT%H%M%SZ")
    campaign = result_root.resolve() / identifier
    campaign.mkdir(parents=True)
    (campaign / "runs").mkdir()
    (campaign / "logs").mkdir()
    return campaign


def create_campaign(
    campaign: Path, arguments: argparse.Namespace, points: list[dict[str, Any]]
) -> dict[str, Any]:
    state = {
        "schema_version": SCHEMA_VERSION,
        "created_at": utc_now(),
        "updated_at": utc_now(),
        "configuration": campaign_configuration(arguments),
        "points": points,
    }
    atomic_json(campaign / "campaign-state.json", state)
    atomic_json(
        campaign / "campaign-plan.json",
        {
            "schema_version": SCHEMA_VERSION,
            "created_at": state["created_at"],
            "configuration": state["configuration"],
            "points": [{key: value for key, value in point.items() if key != "status"} for point in points],
        },
    )
    return state


def save_state(campaign: Path, state: dict[str, Any]) -> None:
    state["updated_at"] = utc_now()
    atomic_json(campaign / "campaign-state.json", state)


def execute_campaign(
    campaign: Path,
    state: dict[str, Any],
    continue_on_error: bool,
    retry_failed: bool,
) -> int:
    configuration = state["configuration"]
    binary = Path(configuration["binary"])
    driver = Path(configuration["driver"])
    run_root = campaign / "runs"
    needs_audio = any(
        point["audio"] != "none"
        and (point["status"] in {"pending", "interrupted"} or (retry_failed and point["status"] == "failed"))
        for point in state["points"]
    )
    voice_file = (
        prepare_voice_file(campaign, Path(configuration["voice_fixture"])) if needs_audio else None
    )
    preflight(
        state["points"],
        binary,
        driver,
        bool(configuration["skip_nofile_check"]),
    )
    selected = [
        point
        for point in state["points"]
        if point["status"] in {"pending", "interrupted"}
        or (retry_failed and point["status"] == "failed")
    ]
    for index, point in enumerate(selected, start=1):
        eligible = point["status"] in {"pending", "interrupted"} or (
            retry_failed and point["status"] == "failed"
        )
        if not eligible:
            continue
        command = command_for_point(
            point,
            binary,
            driver,
            run_root,
            voice_file,
            configuration["duration"],
            configuration["ramp"],
        )
        point["status"] = "running"
        point["attempts"] = int(point.get("attempts", 0)) + 1
        point["started_at"] = utc_now()
        point["command"] = command
        save_state(campaign, state)
        emit_event(campaign, "point_started", point_id=point["id"], attempt=point["attempts"])
        print(f"\n[{index}/{len(selected)}] {point['id']}")
        print("$ " + " ".join(command))
        before = run_directories(run_root)
        try:
            return_code = run_logged(command, campaign / "logs" / f"{point['id']}.log")
        except KeyboardInterrupt:
            point["status"] = "interrupted"
            point["finished_at"] = utc_now()
            save_state(campaign, state)
            emit_event(campaign, "point_interrupted", point_id=point["id"])
            generate_reports(campaign, run_root, discover_run_directory(before, run_root))
            print(f"\nInterrupted. Resume with: {sys.executable} {Path(__file__)} resume {campaign}")
            return 130
        run_directory = discover_run_directory(before, run_root)
        if run_directory is not None:
            point["run_directory"] = str(run_directory.relative_to(campaign))
        point["return_code"] = return_code
        point["finished_at"] = utc_now()
        point["status"] = "succeeded" if return_code == 0 else "failed"
        save_state(campaign, state)
        emit_event(
            campaign,
            "point_finished",
            point_id=point["id"],
            status=point["status"],
            return_code=return_code,
        )
        generate_reports(campaign, run_root, run_directory)
        if return_code:
            if not continue_on_error:
                print(f"Stopped after {point['id']} failed with exit code {return_code}")
                return return_code
        cooldown = float(configuration["cooldown_seconds"])
        if cooldown and index < len(selected):
            print(f"Cooling down for {cooldown:g}s")
            time.sleep(cooldown)
    generate_reports(campaign, run_root)
    return 1 if any(point["status"] == "failed" for point in state["points"]) else 0


def load_campaign(campaign: Path) -> dict[str, Any]:
    with (campaign / "campaign-state.json").open(encoding="utf-8") as source:
        state = json.load(source)
    if not isinstance(state, dict) or int(state.get("schema_version", 0)) != SCHEMA_VERSION:
        raise ValueError("unsupported campaign-state.json schema_version")
    for point in state.get("points", []):
        if point.get("status") == "running":
            point["status"] = "interrupted"
    save_state(campaign, state)
    return state


def add_plan_arguments(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--profile", choices=("quick", "capacity", "full"), default="capacity")
    parser.add_argument(
        "--cardinalities", type=parse_cardinalities, default=sorted(ALLOWED_CARDINALITIES)
    )
    parser.add_argument("--max-participants", type=int, default=LARGE_LOAD_THRESHOLD)
    parser.add_argument("--seeds", type=parse_csv_integers, default=[42])
    parser.add_argument("--controllers", type=parse_controllers, default="auto")
    parser.add_argument("--binary", type=Path, default=DEFAULT_BINARY)
    parser.add_argument("--matrix-file", type=Path)


def parse_arguments(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="command", required=True)

    plan = subcommands.add_parser("plan", help="print the selected official matrix points")
    add_plan_arguments(plan)

    run = subcommands.add_parser("run", help="create and execute a resumable campaign")
    add_plan_arguments(run)
    run.add_argument("--result-root", type=Path, default=Path("/var/tmp/mumble-spaces-campaigns"))
    run.add_argument("--driver", type=Path, default=DEFAULT_DRIVER)
    run.add_argument("--voice-fixture", type=Path, default=DEFAULT_FIXTURE)
    run.add_argument("--duration", type=parse_duration, default="30s")
    run.add_argument("--ramp", type=parse_duration, default="30s")
    run.add_argument("--cooldown", type=float, default=10.0)
    run.add_argument("--skip-build", action="store_true")
    run.add_argument("--skip-nofile-check", action="store_true")
    run.add_argument("--continue-on-error", action="store_true")
    run.add_argument("--ack-large-load", action="store_true")
    run.add_argument("--dry-run", action="store_true")

    resume = subcommands.add_parser("resume", help="resume an interrupted campaign")
    resume.add_argument("campaign", type=Path)
    resume.add_argument("--continue-on-error", action="store_true")
    resume.add_argument("--retry-failed", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    try:
        if arguments.command == "resume":
            campaign = arguments.campaign.expanduser().resolve()
            state = load_campaign(campaign)
            return execute_campaign(
                campaign, state, arguments.continue_on_error, arguments.retry_failed
            )

        if arguments.max_participants < 1:
            raise ValueError("--max-participants must be positive")
        campaign = None
        if arguments.command == "run" and not arguments.dry_run:
            if arguments.max_participants > LARGE_LOAD_THRESHOLD and not arguments.ack_large_load:
                raise ValueError(
                    f"loads above {LARGE_LOAD_THRESHOLD} clients require --ack-large-load and a "
                    "dedicated authorized host"
                )
            campaign = allocate_campaign(arguments.result_root)
            print(f"Campaign directory: {campaign}")
            if not arguments.skip_build:
                build_components(campaign)
        matrix = load_matrix(arguments.binary.resolve(), arguments.matrix_file)
        points = plan_points(
            matrix,
            arguments.profile,
            arguments.cardinalities,
            arguments.max_participants,
            arguments.seeds,
            arguments.controllers,
        )
        print_plan(points)
        if arguments.command == "plan" or arguments.dry_run:
            return 0
        if campaign is None:
            raise RuntimeError("campaign directory was not allocated")
        state = create_campaign(campaign, arguments, points)
        emit_event(campaign, "campaign_created", points=len(points))
        return execute_campaign(campaign, state, arguments.continue_on_error, False)
    except KeyboardInterrupt:
        return 130
    except (OSError, ValueError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
