#!/usr/bin/env python3
"""Generate standalone HTML reports from Spaces headless stress artifacts."""

from __future__ import annotations

import argparse
import csv
import html
import json
import math
import os
import re
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable


SYNC_P99_LIMIT_MILLIS = 2_000.0
PING_P99_LIMIT_MILLIS = 100.0
AUDIO_DELIVERY_MINIMUM_PERCENT = 99.9
MAX_CHART_POINTS = 600
PROCESS_GROUP_LABELS = {
    "coordinator": "Coordinator + embedded server",
    "drivers": "Java drivers (total)",
    "workers": "Mumble workers (total)",
    "other": "Other processes",
}
KNOWN_LOG_FAILURES = (
    (
        re.compile(r"too many open files", re.IGNORECASE),
        "file-descriptor-exhaustion",
        "OS file descriptor limit reached",
    ),
    (
        re.compile(r"credential collection timeout", re.IGNORECASE),
        "credential-timeout",
        "Credential collection timed out",
    ),
    (
        re.compile(r"deadline has elapsed", re.IGNORECASE),
        "deadline",
        "An operation deadline elapsed",
    ),
    (
        re.compile(r"connection refused", re.IGNORECASE),
        "connection-refused",
        "A connection was refused",
    ),
    (
        re.compile(r"address already in use", re.IGNORECASE),
        "address-in-use",
        "A listening address was already in use",
    ),
)


def read_json(path: Path) -> tuple[dict[str, Any] | None, str | None]:
    if not path.is_file():
        return None, None
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        return None, f"Could not read {path.name}: {error}"
    if not isinstance(value, dict):
        return None, f"{path.name} does not contain a JSON object"
    return value, None


def iter_jsonl(path: Path, warnings: list[str]) -> Iterable[dict[str, Any]]:
    if not path.is_file():
        return
    malformed = 0
    try:
        with path.open(encoding="utf-8") as source:
            for line in source:
                if not line.strip():
                    continue
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    malformed += 1
                    continue
                if isinstance(value, dict):
                    yield value
                else:
                    malformed += 1
    except (OSError, UnicodeError) as error:
        warnings.append(f"Could not read {path.name}: {error}")
        return
    if malformed:
        warnings.append(f"Ignored {malformed} malformed record(s) in {path.name}")


def as_number(value: Any) -> float | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, (int, float)) and math.isfinite(float(value)):
        return float(value)
    if isinstance(value, str):
        try:
            parsed = float(value)
        except ValueError:
            return None
        if math.isfinite(parsed):
            return parsed
    return None


def integer(value: Any, default: int = 0) -> int:
    parsed = as_number(value)
    return int(parsed) if parsed is not None else default


def percentile(sorted_values: list[float], ratio: float) -> float | None:
    if not sorted_values:
        return None
    index = math.floor((len(sorted_values) - 1) * ratio)
    return sorted_values[index]


def distribution(values: list[float]) -> dict[str, Any] | None:
    if not values:
        return None
    ordered = sorted(values)
    return {
        "count": len(ordered),
        "min": ordered[0],
        "p50": percentile(ordered, 0.50),
        "p95": percentile(ordered, 0.95),
        "p99": percentile(ordered, 0.99),
        "max": ordered[-1],
    }


def histogram(values: list[float], bin_count: int = 18) -> list[dict[str, Any]]:
    if not values:
        return []
    minimum = min(values)
    maximum = max(values)
    if minimum == maximum:
        return [{"label": f"{minimum:.0f}", "value": len(values)}]
    width = (maximum - minimum) / bin_count
    bins = [0] * bin_count
    for value in values:
        index = min(int((value - minimum) / width), bin_count - 1)
        bins[index] += 1
    return [
        {
            "label": f"{minimum + index * width:.0f}-{minimum + (index + 1) * width:.0f}",
            "value": count,
        }
        for index, count in enumerate(bins)
    ]


def downsample(points: list[list[float]], limit: int = MAX_CHART_POINTS) -> list[list[float]]:
    if len(points) <= limit:
        return points
    sampled = [points[0]]
    interior = limit - 2
    for index in range(interior):
        source_index = 1 + math.floor(index * (len(points) - 2) / interior)
        sampled.append(points[source_index])
    sampled.append(points[-1])
    return sampled


def process_group(role: str) -> tuple[str, str]:
    if role == "coordinator":
        return "coordinator", PROCESS_GROUP_LABELS["coordinator"]
    if role.startswith("driver-"):
        return "drivers", PROCESS_GROUP_LABELS["drivers"]
    if role.startswith("worker-"):
        return "workers", PROCESS_GROUP_LABELS["workers"]
    return "other", PROCESS_GROUP_LABELS["other"]


def read_process_metrics(path: Path, warnings: list[str]) -> dict[str, Any]:
    if not path.is_file():
        return {"roles": [], "series": [], "first_unix_millis": None}
    roles: dict[str, dict[str, float]] = {}
    grouped: dict[int, dict[str, dict[str, float]]] = defaultdict(
        lambda: defaultdict(lambda: {"cpu": 0.0, "rss_mib": 0.0})
    )
    first_timestamp: int | None = None
    try:
        with path.open(encoding="utf-8", newline="") as source:
            for row in csv.DictReader(source):
                timestamp = integer(row.get("unix_millis"), -1)
                role = row.get("role", "")
                rss_kib = as_number(row.get("rss_kib"))
                cpu = as_number(row.get("cpu_percent"))
                if timestamp < 0 or not role or rss_kib is None or cpu is None:
                    continue
                if first_timestamp is None or timestamp < first_timestamp:
                    first_timestamp = timestamp
                rss_mib = rss_kib / 1024.0
                peak = roles.setdefault(role, {"peak_cpu": 0.0, "peak_rss_mib": 0.0})
                peak["peak_cpu"] = max(peak["peak_cpu"], cpu)
                peak["peak_rss_mib"] = max(peak["peak_rss_mib"], rss_mib)
                group, _ = process_group(role)
                grouped[timestamp][group]["cpu"] += cpu
                grouped[timestamp][group]["rss_mib"] += rss_mib
    except (OSError, UnicodeError, csv.Error) as error:
        warnings.append(f"Could not read {path.name}: {error}")

    role_rows = []
    for role, peaks in roles.items():
        group, group_label = process_group(role)
        role_rows.append({"role": role, "group": group, "group_label": group_label, **peaks})
    role_rows.sort(key=lambda item: (-item["peak_cpu"], item["role"]))

    series: dict[str, dict[str, list[list[float]]]] = defaultdict(
        lambda: {"cpu": [], "rss_mib": []}
    )
    if first_timestamp is not None:
        for timestamp in sorted(grouped):
            seconds = (timestamp - first_timestamp) / 1000.0
            for group, values in grouped[timestamp].items():
                series[group]["cpu"].append([seconds, values["cpu"]])
                series[group]["rss_mib"].append([seconds, values["rss_mib"]])
    return {
        "roles": role_rows,
        "series": [
            {
                "key": group,
                "label": PROCESS_GROUP_LABELS[group],
                "cpu": downsample(values["cpu"]),
                "rss_mib": downsample(values["rss_mib"]),
            }
            for group, values in sorted(series.items())
        ],
        "first_unix_millis": first_timestamp,
    }


def counter_rate(previous: dict[str, Any], current: dict[str, Any], path: tuple[str, ...]) -> float:
    elapsed = (integer(current.get("unix_millis")) - integer(previous.get("unix_millis"))) / 1000.0
    if elapsed <= 0:
        return 0.0
    before: Any = previous
    after: Any = current
    for part in path:
        before = before.get(part, {}) if isinstance(before, dict) else 0
        after = after.get(part, {}) if isinstance(after, dict) else 0
    return max(0.0, (integer(after) - integer(before)) / elapsed)


def read_server_metrics(path: Path, warnings: list[str]) -> dict[str, Any]:
    samples = list(iter_jsonl(path, warnings))
    if not samples:
        return {"samples": 0, "maxima": {}, "final": {}, "series": {}}
    first_millis = integer(samples[0].get("unix_millis"))
    state_series = {"connections": [], "participants": [], "spaces": []}
    queue_series = {"depth": [], "maximum": []}
    rate_series = {"commands": [], "responses": [], "reconciliations": []}
    reconcile_series = {"maximum_millis": []}

    for index, sample in enumerate(samples):
        timestamp = (integer(sample.get("unix_millis")) - first_millis) / 1000.0
        actor = sample.get("actor") if isinstance(sample.get("actor"), dict) else {}
        state_series["connections"].append([timestamp, integer(sample.get("connections"))])
        state_series["participants"].append([timestamp, integer(actor.get("participants"))])
        state_series["spaces"].append([timestamp, integer(actor.get("spaces"))])
        queue_series["depth"].append([timestamp, integer(actor.get("queue_depth"))])
        queue_series["maximum"].append([timestamp, integer(actor.get("queue_depth_max"))])
        reconcile_series["maximum_millis"].append(
            [timestamp, integer(actor.get("reconciliation_max_nanos")) / 1_000_000.0]
        )
        if index:
            previous = samples[index - 1]
            rate_series["commands"].append([timestamp, counter_rate(previous, sample, ("actor", "commands"))])
            rate_series["responses"].append([timestamp, counter_rate(previous, sample, ("actor", "responses"))])
            rate_series["reconciliations"].append(
                [timestamp, counter_rate(previous, sample, ("actor", "reconciliations"))]
            )

    actor_samples = [sample.get("actor", {}) for sample in samples]
    maxima = {
        "connections": max(integer(sample.get("connections")) for sample in samples),
        "participants": max(integer(actor.get("participants")) for actor in actor_samples),
        "spaces": max(integer(actor.get("spaces")) for actor in actor_samples),
        "queue_depth": max(integer(actor.get("queue_depth")) for actor in actor_samples),
        "queue_depth_max": max(integer(actor.get("queue_depth_max")) for actor in actor_samples),
        "queue_saturations": max(integer(actor.get("queue_saturations")) for actor in actor_samples),
        "reconciliation_max_millis": max(
            integer(actor.get("reconciliation_max_nanos")) / 1_000_000.0 for actor in actor_samples
        ),
    }
    final_sample = samples[-1]
    final_actor = final_sample.get("actor") if isinstance(final_sample.get("actor"), dict) else {}
    final = {
        "commands": integer(final_actor.get("commands")),
        "responses": integer(final_actor.get("responses")),
        "reconciliations": integer(final_actor.get("reconciliations")),
        "refused_publications": integer(final_actor.get("refused_publications")),
        "lease_expirations": integer(final_actor.get("lease_expirations")),
        "audio_ingress_packets": integer(final_sample.get("audio_ingress_packets")),
        "audio_egress_packets": integer(final_sample.get("audio_egress_packets")),
        "audio_fanout_deliveries": integer(final_sample.get("audio_fanout_deliveries")),
        "audio_dropped_packets": integer(final_sample.get("audio_dropped_packets")),
    }
    return {
        "samples": len(samples),
        "maxima": maxima,
        "final": final,
        "series": {
            "state": {key: downsample(points) for key, points in state_series.items()},
            "queue": {key: downsample(points) for key, points in queue_series.items()},
            "rate": {key: downsample(points) for key, points in rate_series.items()},
            "reconciliation": {
                key: downsample(points) for key, points in reconcile_series.items()
            },
        },
    }


def read_events(run_directory: Path, warnings: list[str]) -> dict[str, Any]:
    counts: Counter[str] = Counter()
    phases: list[dict[str, Any]] = []
    for record in iter_jsonl(run_directory / "events.jsonl", warnings):
        kind = record.get("kind")
        if isinstance(kind, str):
            counts[kind] += 1
        if kind == "failure_phase" and isinstance(record.get("phase"), str):
            phases.append(
                {
                    "phase": record["phase"],
                    "unix_nanos": str(record.get("coordinator_unix_nanos", "")),
                }
            )
    return {
        "counts": [
            {"label": kind, "value": count}
            for kind, count in counts.most_common(14)
        ],
        "total": sum(counts.values()),
        "phases": phases,
    }


def read_worker_events(run_directory: Path, warnings: list[str]) -> dict[str, Any]:
    counts: Counter[str] = Counter()
    synchronization_millis: list[float] = []
    files = sorted(run_directory.glob("worker-*.jsonl"))
    for path in files:
        for record in iter_jsonl(path, warnings):
            event = record.get("event") if isinstance(record.get("event"), dict) else {}
            kind = event.get("kind")
            if not isinstance(kind, str):
                continue
            counts[kind] += 1
            if kind == "synchronized":
                monotonic_micros = as_number(event.get("monotonic_micros"))
                if monotonic_micros is not None and monotonic_micros >= 0:
                    synchronization_millis.append(monotonic_micros / 1000.0)
    return {
        "files": len(files),
        "counts": [{"label": kind, "value": count} for kind, count in counts.most_common()],
        "synchronization": distribution(synchronization_millis),
        "synchronization_histogram": histogram(synchronization_millis),
    }


def read_worker_reports(run_directory: Path, warnings: list[str]) -> dict[str, Any]:
    files = sorted(run_directory.glob("worker-*-report.json"))
    totals: Counter[str] = Counter()
    tcp_ping_p99_micros: list[float] = []
    udp_ping_p99_micros: list[float] = []
    for path in files:
        report, error = read_json(path)
        if error:
            warnings.append(error)
            continue
        if report is None:
            continue
        stats = report.get("stats") if isinstance(report.get("stats"), dict) else {}
        totals["clients_expected"] += integer(report.get("clients_expected"))
        for name in (
            "clients_reported",
            "clients_completed",
            "tcp_frames_received",
            "tcp_pings_sent",
            "udp_packets_sent",
            "udp_packets_received",
            "voice_packets_sent",
            "voice_packets_received",
            "interactions_sent",
            "denied_interactions",
            "reconnects",
        ):
            totals[name] += integer(stats.get(name))
        for name, destination in (
            ("tcp_ping_rtt", tcp_ping_p99_micros),
            ("udp_ping_rtt", udp_ping_p99_micros),
        ):
            latency = stats.get(name) if isinstance(stats.get(name), dict) else {}
            p99 = as_number(latency.get("p99_micros"))
            if p99 is not None:
                destination.append(p99)
    clients_expected = totals["clients_expected"]
    return {
        "files": len(files),
        **dict(totals),
        "failure_rate": (
            max(0, clients_expected - totals["clients_completed"]) / clients_expected
            if clients_expected
            else 0.0
        ),
        "maximum_worker_tcp_ping_p99_micros": (
            max(tcp_ping_p99_micros) if tcp_ping_p99_micros else None
        ),
        "maximum_worker_udp_ping_p99_micros": (
            max(udp_ping_p99_micros) if udp_ping_p99_micros else None
        ),
    }


def scan_known_failures(run_directory: Path, warnings: list[str]) -> list[dict[str, Any]]:
    failures: dict[str, dict[str, Any]] = {}
    for path in sorted(run_directory.glob("*.log")):
        try:
            with path.open(encoding="utf-8", errors="replace") as source:
                for line in source:
                    for pattern, code, message in KNOWN_LOG_FAILURES:
                        if not pattern.search(line):
                            continue
                        failure = failures.setdefault(
                            code,
                            {"code": code, "message": message, "files": set(), "occurrences": 0},
                        )
                        failure["files"].add(path.name)
                        failure["occurrences"] += 1
        except OSError as error:
            warnings.append(f"Could not scan {path.name}: {error}")
    return [
        {
            **failure,
            "files": sorted(failure["files"]),
        }
        for failure in failures.values()
    ]


def safe_manifest(manifest: dict[str, Any] | None) -> dict[str, Any]:
    if manifest is None:
        return {}
    allowed = (
        "schema_version",
        "git_sha",
        "git_dirty",
        "operating_system",
        "architecture",
        "available_parallelism",
        "mode",
        "scenario",
        "audio",
        "fault",
        "controllers",
        "participants",
        "participants_per_space",
        "seed",
    )
    return {key: manifest.get(key) for key in allowed if key in manifest}


def safe_summary(summary: dict[str, Any] | None) -> dict[str, Any]:
    if summary is None:
        return {}
    allowed = (
        "scenario",
        "controllers",
        "participants_requested",
        "credentials_received",
        "workers",
        "driver_events",
        "credential_rotations",
        "ownership_losses",
        "ownership_violations",
        "stale_credential_rejections",
        "recovery_audits",
        "errors",
        "elapsed_millis",
    )
    safe = {key: summary.get(key) for key in allowed if key in summary}
    mumble = summary.get("mumble")
    if isinstance(mumble, dict):
        mumble_allowed = (
            "processes_spawned",
            "reports_expected",
            "reports_received",
            "interrupted_processes",
            "missing_reports",
            "process_exit_failures",
            "clients_expected",
            "clients_reported",
            "clients_completed",
            "failure_rate",
            "maximum_worker_tcp_ping_p99_micros",
            "maximum_worker_udp_ping_p99_micros",
            "tcp_frames_received",
            "tcp_pings_sent",
            "udp_packets_sent",
            "udp_packets_received",
            "voice_packets_sent",
            "voice_packets_received",
            "interactions_sent",
            "denied_interactions",
            "reconnects",
        )
        safe["mumble"] = {
            key: mumble.get(key) for key in mumble_allowed if key in mumble
        }
    return safe


def health_status(
    manifest: dict[str, Any],
    summary: dict[str, Any],
    server: dict[str, Any],
    workers: dict[str, Any],
    failures: list[dict[str, Any]],
) -> tuple[dict[str, str], list[dict[str, str]]]:
    checks: list[dict[str, str]] = []

    def add_check(name: str, status: str, detail: str) -> None:
        checks.append({"name": name, "status": status, "detail": detail})

    if summary:
        violations = integer(summary.get("ownership_violations"))
        add_check(
            "Ownership and fencing",
            "pass" if violations == 0 else "fail",
            f"{violations} ownership violation(s)",
        )
        errors = integer(summary.get("errors"))
        add_check(
            "Coordinator errors",
            "pass" if errors == 0 else "fail",
            f"{errors} reported error(s)",
        )
    else:
        add_check("Run completion", "unknown", "summary.json is absent")

    synchronization = workers.get("synchronization")
    if isinstance(synchronization, dict) and synchronization.get("p99") is not None:
        p99 = float(synchronization["p99"])
        add_check(
            "Mumble synchronization p99",
            "pass" if p99 < SYNC_P99_LIMIT_MILLIS else "fail",
            f"{p99:.1f} ms, provisional limit {SYNC_P99_LIMIT_MILLIS:.0f} ms",
        )
    else:
        add_check("Mumble synchronization p99", "unknown", "No synchronized worker event")

    maxima = server.get("maxima") if isinstance(server.get("maxima"), dict) else {}
    if server.get("samples"):
        saturations = integer(maxima.get("queue_saturations"))
        add_check(
            "Actor queue saturation",
            "pass" if saturations == 0 else "fail",
            f"{saturations} saturation event(s)",
        )
    else:
        add_check("Actor queue saturation", "unknown", "No server metrics")

    aggregate = workers.get("aggregate") if isinstance(workers.get("aggregate"), dict) else {}
    if aggregate:
        missing = integer(aggregate.get("missing_reports"))
        exits = integer(aggregate.get("process_exit_failures"))
        completed = integer(aggregate.get("clients_completed"))
        expected = integer(aggregate.get("clients_expected"))
        completion_ok = missing == 0 and exits == 0 and completed == expected
        add_check(
            "Mumble client completion",
            "pass" if completion_ok else "fail",
            f"{completed}/{expected} completed; {missing} missing report(s); {exits} failed worker process(es)",
        )
        ping_values = [
            value / 1000.0
            for value in (
                as_number(aggregate.get("maximum_worker_tcp_ping_p99_micros")),
                as_number(aggregate.get("maximum_worker_udp_ping_p99_micros")),
            )
            if value is not None
        ]
        if ping_values:
            maximum_ping = max(ping_values)
            add_check(
                "Mumble ping p99",
                "pass" if maximum_ping < PING_P99_LIMIT_MILLIS else "fail",
                f"{maximum_ping:.1f} ms maximum worker p99, provisional limit {PING_P99_LIMIT_MILLIS:.0f} ms",
            )
        else:
            add_check("Mumble ping p99", "unknown", "No worker ping samples")
    else:
        add_check("Mumble client completion", "unknown", "No worker aggregate report")
        add_check("Mumble ping p99", "unknown", "No worker aggregate report")

    scenario = str(manifest.get("scenario", "")).lower()
    if scenario == "voice":
        final = server.get("final") if isinstance(server.get("final"), dict) else {}
        fanout = integer(final.get("audio_fanout_deliveries"))
        received = integer(aggregate.get("voice_packets_received"))
        if fanout > 0:
            delivery = received * 100.0 / fanout
            add_check(
                "Audio delivery",
                "pass" if delivery >= AUDIO_DELIVERY_MINIMUM_PERCENT else "fail",
                f"{delivery:.3f}% observed ({received}/{fanout}), provisional minimum {AUDIO_DELIVERY_MINIMUM_PERCENT:.1f}%",
            )
        else:
            add_check("Audio delivery", "unknown", "No server audio fanout samples")

    if failures:
        return (
            {"code": "incomplete", "label": "INCOMPLETE", "detail": failures[0]["message"]},
            checks,
        )
    if not summary:
        return (
            {"code": "incomplete", "label": "INCOMPLETE", "detail": "The run did not write a summary"},
            checks,
        )
    failed = [check for check in checks if check["status"] == "fail"]
    if failed:
        return (
            {"code": "fail", "label": "ISSUES DETECTED", "detail": failed[0]["name"]},
            checks,
        )
    unknown = [check for check in checks if check["status"] == "unknown"]
    if unknown:
        return (
            {
                "code": "partial",
                "label": "PARTIAL PASS",
                "detail": "Available checks pass; some signals were not persisted",
            },
            checks,
        )
    return ({"code": "pass", "label": "PASS", "detail": "All available checks pass"}, checks)


def artifact_links(run_directory: Path, output_directory: Path) -> list[dict[str, str]]:
    names = (
        "manifest.json",
        "summary.json",
        "summary.csv",
        "events.jsonl",
        "server-metrics.jsonl",
        "process-metrics.csv",
        "server.log",
    )
    links = []
    for name in names:
        path = run_directory / name
        if path.is_file():
            links.append({"name": name, "href": os.path.relpath(path, output_directory)})
    for path in sorted(run_directory.glob("worker-*-report.json")):
        links.append(
            {"name": path.name, "href": os.path.relpath(path, output_directory)}
        )
    return links


def analyze_run(run_directory: Path, output_directory: Path | None = None) -> dict[str, Any]:
    warnings: list[str] = []
    manifest_raw, manifest_error = read_json(run_directory / "manifest.json")
    summary_raw, summary_error = read_json(run_directory / "summary.json")
    if manifest_error:
        warnings.append(manifest_error)
    if summary_error:
        warnings.append(summary_error)
    manifest = safe_manifest(manifest_raw)
    summary = safe_summary(summary_raw)
    processes = read_process_metrics(run_directory / "process-metrics.csv", warnings)
    server = read_server_metrics(run_directory / "server-metrics.jsonl", warnings)
    events = read_events(run_directory, warnings)
    workers = read_worker_events(run_directory, warnings)
    persisted_worker_reports = read_worker_reports(run_directory, warnings)
    summary_worker_aggregate = summary.get("mumble")
    workers["persisted_reports"] = persisted_worker_reports
    workers["aggregate"] = (
        summary_worker_aggregate
        if isinstance(summary_worker_aggregate, dict)
        else persisted_worker_reports
    )
    failures = scan_known_failures(run_directory, warnings)
    status, checks = health_status(manifest, summary, server, workers, failures)
    output_directory = output_directory or run_directory
    participants_requested = integer(
        summary.get("participants_requested"), integer(manifest.get("participants"))
    )
    return {
        "schema_version": 1,
        "report_generated_at": datetime.now(timezone.utc).isoformat(),
        "run_name": run_directory.name,
        "run_path": str(run_directory),
        "manifest": manifest,
        "summary": summary,
        "participants_requested": participants_requested,
        "status": status,
        "checks": checks,
        "failures": failures,
        "warnings": warnings,
        "processes": processes,
        "server": server,
        "events": events,
        "workers": workers,
        "artifacts": artifact_links(run_directory, output_directory),
    }


REPORT_HTML = r'''<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width,initial-scale=1">
  <title>__TITLE__</title>
  <style>
    :root { --text:#222; --muted:#666; --line:#d4d4d4; --header:#efefef; --stripe:#fafafa; --link:#1a5fb4; --green:#2e7d32; --green-bg:#e8f5e9; --amber:#8a6700; --amber-bg:#fff8df; --red:#b71c1c; --red-bg:#ffebee; }
    * { box-sizing:border-box; }
    body { margin:0; background:#fff; color:var(--text); font:13px/1.45 Arial,Helvetica,sans-serif; }
    main { width:min(1400px,calc(100% - 32px)); margin:0 auto; padding:20px 0 48px; }
    h1,h2,h3,p { margin-top:0; } h1 { font-size:24px; font-weight:normal; margin-bottom:5px; } h2 { font-size:16px; margin-bottom:12px; } h3 { font-size:13px; color:var(--muted); margin-bottom:8px; }
    .eyebrow { color:var(--muted); font-size:11px; font-weight:bold; text-transform:uppercase; }
    .hero { display:flex; align-items:flex-end; justify-content:space-between; gap:24px; margin-bottom:18px; padding-bottom:12px; border-bottom:1px solid var(--line); }
    .subtitle { color:var(--muted); margin:0; overflow-wrap:anywhere; }
    .status { min-width:220px; border:1px solid var(--line); border-left-width:6px; padding:9px 11px; background:#fafafa; }
    .status strong { display:block; font-size:14px; } .status span { color:var(--muted); }
    .status.pass { border-left-color:var(--green); background:var(--green-bg); } .status.pass strong { color:var(--green); }
    .status.partial { border-left-color:var(--amber); background:var(--amber-bg); } .status.partial strong { color:var(--amber); }
    .status.fail,.status.incomplete { border-left-color:var(--red); background:var(--red-bg); } .status.fail strong,.status.incomplete strong { color:var(--red); }
    .cards { display:grid; grid-template-columns:repeat(6,minmax(140px,1fr)); gap:8px; margin-bottom:16px; }
    .card,.panel { background:#fff; border:1px solid var(--line); }
    .card { padding:11px; min-height:88px; } .card .label { color:var(--muted); font-size:11px; font-weight:bold; } .card .value { display:block; font-size:22px; font-weight:bold; margin-top:5px; } .card .hint { color:var(--muted); font-size:11px; }
    .grid { display:grid; grid-template-columns:repeat(2,minmax(0,1fr)); gap:12px; margin-bottom:20px; } .panel { padding:14px; min-width:0; } .wide { grid-column:1/-1; }
    .chart { min-height:260px; } svg { width:100%; height:auto; display:block; overflow:visible; } .axis { stroke:var(--line); stroke-width:1; } .gridline { stroke:#e5e5e5; stroke-width:1; } .tick { fill:var(--muted); font-size:11px; } .legend { display:flex; flex-wrap:wrap; gap:14px; color:var(--muted); margin:-2px 0 8px; font-size:11px; } .legend i { width:10px; height:3px; display:inline-block; margin:0 5px 3px 0; }
    table { width:100%; border-collapse:collapse; } th,td { padding:7px 9px; border:1px solid var(--line); text-align:left; white-space:nowrap; } th { background:var(--header); color:#444; font-size:11px; } tbody tr:nth-child(even) { background:var(--stripe); } td:first-child { white-space:normal; }
    .scroll { overflow-x:auto; } .badge { display:inline-block; border:1px solid currentColor; padding:1px 5px; font-size:10px; font-weight:bold; text-transform:uppercase; } .badge.pass { color:var(--green); background:var(--green-bg); } .badge.fail { color:var(--red); background:var(--red-bg); } .badge.unknown { color:var(--amber); background:var(--amber-bg); }
    .notice { padding:8px 10px; border:1px solid #e1c96a; background:var(--amber-bg); color:#5f4a00; margin:9px 0; }
    .failure { border-color:#d9a3a3; background:var(--red-bg); color:var(--red); }
    .meta { display:grid; grid-template-columns:repeat(4,minmax(0,1fr)); border:1px solid var(--line); border-width:1px 0 0 1px; } .meta div { padding:9px; border:1px solid var(--line); border-width:0 1px 1px 0; } .meta dt { color:var(--muted); font-size:11px; font-weight:bold; } .meta dd { margin:2px 0 0; overflow-wrap:anywhere; }
    .artifacts { display:flex; gap:6px; flex-wrap:wrap; } a { color:var(--link); text-decoration:none; } a:hover { text-decoration:underline; } .artifact { border:1px solid var(--line); background:#f7f7f7; padding:4px 7px; }
    .footnote { color:var(--muted); font-size:12px; margin-top:18px; }
    @media (max-width:1050px) { .cards { grid-template-columns:repeat(3,1fr); } .meta { grid-template-columns:repeat(2,1fr); } }
    @media (max-width:720px) { main { width:min(100% - 20px,1400px); padding-top:12px; } .hero { display:block; } .status { margin-top:12px; } .grid { grid-template-columns:1fr; } .wide { grid-column:auto; } .cards { grid-template-columns:repeat(2,1fr); } .meta { grid-template-columns:1fr; } }
  </style>
</head>
<body><main>
  <header class="hero"><div><div class="eyebrow">Spaces headless load report</div><h1 id="run-title"></h1><p class="subtitle" id="run-path"></p></div><div class="status" id="status"><strong></strong><span></span></div></header>
  <section class="cards" id="cards"></section>
  <section class="grid">
    <article class="panel"><h2>Server state</h2><div id="state-chart" class="chart"></div></article>
    <article class="panel"><h2>Actor queue</h2><div id="queue-chart" class="chart"></div></article>
    <article class="panel"><h2>Process CPU</h2><div id="cpu-chart" class="chart"></div></article>
    <article class="panel"><h2>Process RSS</h2><div id="rss-chart" class="chart"></div></article>
    <article class="panel"><h2>Runtime throughput</h2><div id="rate-chart" class="chart"></div></article>
    <article class="panel"><h2>Maximum reconciliation duration</h2><div id="reconciliation-chart" class="chart"></div></article>
    <article class="panel"><h2>Mumble synchronization distribution</h2><div id="sync-chart" class="chart"></div></article>
    <article class="panel"><h2>Driver events</h2><div id="events-chart" class="chart"></div></article>
    <article class="panel wide"><h2>Health checks</h2><div class="scroll"><table><thead><tr><th>Check</th><th>Status</th><th>Detail</th></tr></thead><tbody id="checks"></tbody></table></div><div id="notices"></div></article>
    <article class="panel wide"><h2>Process peaks</h2><div class="scroll"><table><thead><tr><th>Role</th><th>Peak RSS</th><th>Peak CPU</th></tr></thead><tbody id="process-table"></tbody></table></div></article>
    <article class="panel wide"><h2>Run identity</h2><dl class="meta" id="meta"></dl><h3 style="margin-top:18px">Raw artifacts</h3><div class="artifacts" id="artifacts"></div><p class="footnote">The report embeds only allow-listed metadata and aggregated measurements. Raw logs are linked, not embedded. macOS process CPU can exceed 100% when a process uses multiple cores; the coordinator role includes the embedded managed server.</p></article>
  </section>
</main>
<script id="report-data" type="application/json">__DATA__</script>
<script>
const data=JSON.parse(document.getElementById('report-data').textContent);
const colors=['#4f81bd','#4d9221','#d17c00','#c62828','#6b7280','#7b5ea7'];
const fmt=(value,digits=0)=>value==null?'n/a':Number(value).toLocaleString(undefined,{maximumFractionDigits:digits});
const el=(name,text,cls)=>{const node=document.createElement(name);if(text!=null)node.textContent=text;if(cls)node.className=cls;return node};
document.getElementById('run-title').textContent=data.run_name;
document.getElementById('run-path').textContent=data.run_path;
const status=document.getElementById('status');status.classList.add(data.status.code);status.querySelector('strong').textContent=data.status.label;status.querySelector('span').textContent=data.status.detail;
const sync=data.workers.synchronization||{};const maxima=data.server.maxima||{};const summary=data.summary||{};const mumble=data.workers.aggregate||{};
const cards=[
 ['Participants',fmt(data.participants_requested),`${fmt(maxima.connections)} max connections`],
 ['Sync p99',sync.p99==null?'n/a':`${fmt(sync.p99,1)} ms`,`${fmt(sync.count)} clients observed`],
 ['Mumble clients',`${fmt(mumble.clients_completed)}/${fmt(mumble.clients_expected)}`,`${fmt(mumble.missing_reports)} missing worker reports`],
 ['Max actor queue',fmt(maxima.queue_depth_max),`${fmt(maxima.queue_saturations)} saturations`],
 ['Elapsed',summary.elapsed_millis==null?'n/a':`${fmt(summary.elapsed_millis/1000,1)} s`,`${fmt(data.server.samples)} server samples`],
 ['Ownership violations',fmt(summary.ownership_violations),`${fmt(summary.ownership_losses)} ownership losses`],
 ['Errors',fmt(summary.errors),`${fmt((data.server.final||{}).refused_publications)} refused publications`]
];
for(const item of cards){const card=el('article',null,'card');card.append(el('span',item[0],'label'),el('span',item[1],'value'),el('span',item[2],'hint'));document.getElementById('cards').append(card)}
function legend(container,series){const box=el('div',null,'legend');series.forEach((item,index)=>{const span=el('span');const dot=el('i');dot.style.background=colors[index%colors.length];span.append(dot,document.createTextNode(item.label));box.append(span)});container.append(box)}
function lineChart(id,series,unit=''){
 const container=document.getElementById(id);series=series.filter(item=>item.points&&item.points.length);if(!series.length){container.append(el('div','No samples available','notice'));return}legend(container,series);
 const width=820,height=260,pad={l:62,r:18,t:12,b:38};const points=series.flatMap(item=>item.points);const maxX=Math.max(1,...points.map(p=>p[0]));const maxY=Math.max(1,...points.map(p=>p[1]));const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox',`0 0 ${width} ${height}`);
 const sx=x=>pad.l+x/maxX*(width-pad.l-pad.r),sy=y=>height-pad.b-y/maxY*(height-pad.t-pad.b);
 for(let i=0;i<=4;i++){const y=pad.t+i*(height-pad.t-pad.b)/4;const line=document.createElementNS(svg.namespaceURI,'line');line.setAttribute('x1',pad.l);line.setAttribute('x2',width-pad.r);line.setAttribute('y1',y);line.setAttribute('y2',y);line.setAttribute('class','gridline');svg.append(line);const label=document.createElementNS(svg.namespaceURI,'text');label.setAttribute('x',pad.l-8);label.setAttribute('y',y+4);label.setAttribute('text-anchor','end');label.setAttribute('class','tick');label.textContent=fmt(maxY*(4-i)/4,1)+unit;svg.append(label)}
 for(let i=0;i<=4;i++){const x=pad.l+i*(width-pad.l-pad.r)/4;const label=document.createElementNS(svg.namespaceURI,'text');label.setAttribute('x',x);label.setAttribute('y',height-12);label.setAttribute('text-anchor','middle');label.setAttribute('class','tick');label.textContent=fmt(maxX*i/4,1)+'s';svg.append(label)}
 series.forEach((item,index)=>{const path=document.createElementNS(svg.namespaceURI,'polyline');path.setAttribute('points',item.points.map(p=>`${sx(p[0])},${sy(p[1])}`).join(' '));path.setAttribute('fill','none');path.setAttribute('stroke',colors[index%colors.length]);path.setAttribute('stroke-width','2.4');path.setAttribute('stroke-linejoin','round');path.setAttribute('stroke-linecap','round');svg.append(path)});container.append(svg)
}
function barChart(id,items){const container=document.getElementById(id);if(!items.length){container.append(el('div','No samples available','notice'));return}const width=820,height=260,pad={l:62,r:14,t:12,b:72};const max=Math.max(1,...items.map(i=>i.value));const svg=document.createElementNS('http://www.w3.org/2000/svg','svg');svg.setAttribute('viewBox',`0 0 ${width} ${height}`);const slot=(width-pad.l-pad.r)/items.length;items.forEach((item,index)=>{const h=item.value/max*(height-pad.t-pad.b);const rect=document.createElementNS(svg.namespaceURI,'rect');rect.setAttribute('x',pad.l+index*slot+2);rect.setAttribute('y',height-pad.b-h);rect.setAttribute('width',Math.max(1,slot-4));rect.setAttribute('height',h);rect.setAttribute('fill','#4f81bd');const title=document.createElementNS(svg.namespaceURI,'title');title.textContent=`${item.label}: ${fmt(item.value)}`;rect.append(title);svg.append(rect);const label=document.createElementNS(svg.namespaceURI,'text');label.setAttribute('x',pad.l+(index+.5)*slot);label.setAttribute('y',height-pad.b+12);label.setAttribute('text-anchor','end');label.setAttribute('transform',`rotate(-38 ${pad.l+(index+.5)*slot} ${height-pad.b+12})`);label.setAttribute('class','tick');label.textContent=item.label;svg.append(label)});container.append(svg)}
const state=data.server.series.state||{};lineChart('state-chart',[{label:'Connections',points:state.connections},{label:'Participants',points:state.participants},{label:'Spaces',points:state.spaces}]);
const queue=data.server.series.queue||{};lineChart('queue-chart',[{label:'Current depth',points:queue.depth},{label:'Cumulative maximum',points:queue.maximum}]);
lineChart('cpu-chart',data.processes.series.map(item=>({label:item.label,points:item.cpu})),'%');lineChart('rss-chart',data.processes.series.map(item=>({label:item.label,points:item.rss_mib})),' MiB');
const rate=data.server.series.rate||{};lineChart('rate-chart',[{label:'Commands/s',points:rate.commands},{label:'Responses/s',points:rate.responses},{label:'Reconciliations/s',points:rate.reconciliations}]);
const reconciliation=data.server.series.reconciliation||{};lineChart('reconciliation-chart',[{label:'Cumulative maximum',points:reconciliation.maximum_millis}],' ms');
barChart('sync-chart',data.workers.synchronization_histogram||[]);barChart('events-chart',data.events.counts||[]);
for(const check of data.checks){const row=el('tr');const badge=el('span',check.status,'badge '+check.status);const statusCell=el('td');statusCell.append(badge);row.append(el('td',check.name),statusCell,el('td',check.detail));document.getElementById('checks').append(row)}
const notices=document.getElementById('notices');for(const failure of data.failures){notices.append(el('div',`${failure.message} (${fmt(failure.occurrences)} occurrences in ${failure.files.join(', ')})`,'notice failure'))}for(const warning of data.warnings){notices.append(el('div',warning,'notice'))}
for(const item of data.processes.roles){const row=el('tr');row.append(el('td',item.role),el('td',`${fmt(item.peak_rss_mib,1)} MiB`),el('td',`${fmt(item.peak_cpu,1)}%`));document.getElementById('process-table').append(row)}
const meta=document.getElementById('meta');const manifest=data.manifest||{};const metaItems=[['Scenario',manifest.scenario||summary.scenario],['Audio',manifest.audio||'none'],['Fault',manifest.fault],['Controllers',manifest.controllers||summary.controllers],['Participants / Space',manifest.participants_per_space],['Seed',manifest.seed],['Mode',manifest.mode],['Platform',[manifest.operating_system,manifest.architecture].filter(Boolean).join(' / ')],['Git SHA',manifest.git_sha]];for(const item of metaItems){const box=el('div');box.append(el('dt',item[0]),el('dd',item[1]==null?'n/a':String(item[1])));meta.append(box)}
for(const item of data.artifacts){const link=el('a',item.name,'artifact');link.href=item.href;document.getElementById('artifacts').append(link)}
</script></body></html>'''


CAMPAIGN_HTML = r'''<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Spaces load campaign</title>
<style>:root{--line:#d4d4d4;--header:#efefef;--muted:#666;--link:#1a5fb4;--green:#2e7d32;--green-bg:#e8f5e9;--amber:#8a6700;--amber-bg:#fff8df;--red:#b71c1c;--red-bg:#ffebee}*{box-sizing:border-box}body{margin:0;background:#fff;color:#222;font:13px/1.45 Arial,Helvetica,sans-serif}main{width:min(1450px,calc(100% - 32px));margin:auto;padding:20px 0 48px}h1{font-size:24px;font-weight:normal;margin:0}.eyebrow{color:var(--muted);font-size:11px;font-weight:bold;text-transform:uppercase}.muted{color:var(--muted)}.cards{display:grid;grid-template-columns:repeat(5,1fr);gap:8px;margin:18px 0}.card,.panel{background:#fff;border:1px solid var(--line)}.card{padding:10px}.card span{display:block;color:var(--muted);font-size:11px;font-weight:bold}.card strong{font-size:22px}.panel{padding:12px}.scroll{overflow:auto}table{width:100%;border-collapse:collapse}th,td{padding:7px 9px;border:1px solid var(--line);text-align:left;white-space:nowrap}th{background:var(--header);color:#444;font-size:11px}tbody tr:nth-child(even){background:#fafafa}a{color:var(--link);text-decoration:none}.badge{display:inline-block;border:1px solid currentColor;padding:1px 5px;font-size:10px;font-weight:bold}.pass{color:var(--green);background:var(--green-bg)}.partial{color:var(--amber);background:var(--amber-bg)}.fail,.incomplete{color:var(--red);background:var(--red-bg)}@media(max-width:800px){.cards{grid-template-columns:repeat(2,1fr)}}</style></head>
<body><main><div class="eyebrow">Spaces headless load campaign</div><h1 id="title"></h1><p class="muted">Standalone reports generated from the artifacts already on disk.</p><section class="cards" id="cards"></section><section class="panel"><div class="scroll"><table><thead><tr><th>Run</th><th>Status</th><th>Scenario</th><th>Audio</th><th>Participants</th><th>Per Space</th><th>Controllers</th><th>Sync p99</th><th>Max queue</th><th>Peak connections</th><th>Elapsed</th></tr></thead><tbody id="runs"></tbody></table></div></section></main>
<script id="campaign-data" type="application/json">__DATA__</script><script>const data=JSON.parse(document.getElementById('campaign-data').textContent);const el=(n,t,c)=>{const x=document.createElement(n);if(t!=null)x.textContent=t;if(c)x.className=c;return x};const fmt=(v,d=0)=>v==null?'n/a':Number(v).toLocaleString(undefined,{maximumFractionDigits:d});document.getElementById('title').textContent=data.root_name;const counts={runs:data.runs.length,pass:0,partial:0,fail:0,incomplete:0};data.runs.forEach(r=>counts[r.status.code]++);for(const [label,value] of [['Runs',counts.runs],['Pass',counts.pass],['Partial pass',counts.partial],['Issues',counts.fail],['Incomplete',counts.incomplete]]){const c=el('div',null,'card');c.append(el('span',label),el('strong',value));document.getElementById('cards').append(c)}for(const run of data.runs){const row=el('tr');const link=el('a',run.run_name);link.href=run.report_href;const first=el('td');first.append(link);const badge=el('span',run.status.label,'badge '+run.status.code);const status=el('td');status.append(badge);row.append(first,status,el('td',run.scenario),el('td',run.audio||'none'),el('td',fmt(run.participants)),el('td',fmt(run.participants_per_space)),el('td',fmt(run.controllers)),el('td',run.sync_p99==null?'n/a':fmt(run.sync_p99,1)+' ms'),el('td',fmt(run.max_queue)),el('td',fmt(run.max_connections)),el('td',run.elapsed_millis==null?'n/a':fmt(run.elapsed_millis/1000,1)+' s'));document.getElementById('runs').append(row)}</script></body></html>'''


def encoded_data(data: dict[str, Any]) -> str:
    return (
        json.dumps(data, separators=(",", ":"), ensure_ascii=False)
        .replace("&", "\\u0026")
        .replace("<", "\\u003c")
        .replace(">", "\\u003e")
    )


def write_run_report(run_directory: Path, output: Path | None = None) -> tuple[Path, dict[str, Any]]:
    output = output or run_directory / "report.html"
    data = analyze_run(run_directory, output.parent)
    document = REPORT_HTML.replace("__TITLE__", html.escape(f"Spaces load report - {run_directory.name}"))
    document = document.replace("__DATA__", encoded_data(data))
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(document, encoding="utf-8")
    return output, data


def campaign_row(data: dict[str, Any], report: Path, campaign_output: Path) -> dict[str, Any]:
    manifest = data["manifest"]
    summary = data["summary"]
    synchronization = data["workers"].get("synchronization") or {}
    maxima = data["server"].get("maxima") or {}
    return {
        "run_name": data["run_name"],
        "report_href": os.path.relpath(report, campaign_output.parent),
        "status": data["status"],
        "scenario": manifest.get("scenario", summary.get("scenario", "n/a")),
        "audio": manifest.get("audio", "none"),
        "participants": data["participants_requested"],
        "participants_per_space": manifest.get("participants_per_space"),
        "controllers": manifest.get("controllers", summary.get("controllers")),
        "sync_p99": synchronization.get("p99"),
        "max_queue": maxima.get("queue_depth_max"),
        "max_connections": maxima.get("connections"),
        "elapsed_millis": summary.get("elapsed_millis"),
    }


def write_campaign_report(root: Path, output: Path | None = None) -> tuple[Path, int]:
    output = output or root / "index.html"
    rows = []
    for run_directory in sorted(path for path in root.iterdir() if path.is_dir()):
        if not (run_directory / "manifest.json").is_file():
            continue
        report, data = write_run_report(run_directory)
        rows.append(campaign_row(data, report, output))
    if not rows:
        raise ValueError(f"no run directory containing manifest.json found below {root}")
    rows.sort(key=lambda row: (integer(row.get("participants")), row["run_name"]))
    campaign = {
        "schema_version": 1,
        "root_name": root.name,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "runs": rows,
    }
    document = CAMPAIGN_HTML.replace("__DATA__", encoded_data(campaign))
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(document, encoding="utf-8")
    return output, len(rows)


def parse_arguments(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Generate standalone HTML reports from Spaces headless stress artifacts"
    )
    parser.add_argument(
        "path",
        type=Path,
        help="a run directory, or a result root containing run directories",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="output HTML path (defaults to report.html for a run or index.html for a root)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    arguments = parse_arguments(sys.argv[1:] if argv is None else argv)
    path = arguments.path.expanduser().resolve()
    output = arguments.output.expanduser().resolve() if arguments.output is not None else None
    if not path.is_dir():
        print(f"error: not a directory: {path}", file=sys.stderr)
        return 2
    try:
        if (path / "manifest.json").is_file():
            report, _ = write_run_report(path, output)
            print(f"Wrote run report: {report}")
        else:
            report, count = write_campaign_report(path, output)
            print(f"Wrote campaign report for {count} run(s): {report}")
    except (OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
