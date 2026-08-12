import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


REPORT_PATH = Path(__file__).parents[1] / "report.py"
SPEC = importlib.util.spec_from_file_location("spaces_stress_report", REPORT_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load report.py")
REPORT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(REPORT)


class ReportTest(unittest.TestCase):
    def make_run(self, root: Path, name: str = "100-Idle-42") -> Path:
        run = root / name
        run.mkdir()
        (run / "manifest.json").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "git_sha": "abc123",
                    "git_dirty": False,
                    "operating_system": "linux",
                    "architecture": "x86_64",
                    "available_parallelism": 8,
                    "mode": "managed",
                    "scenario": "idle",
                    "fault": "none",
                    "controllers": 2,
                    "participants": 8,
                    "participants_per_space": 8,
                    "seed": 42,
                    "credential": "must-not-appear",
                }
            ),
            encoding="utf-8",
        )
        (run / "summary.json").write_text(
            json.dumps(
                {
                    "scenario": "Idle",
                    "controllers": 2,
                    "participants_requested": 8,
                    "credentials_received": 8,
                    "workers": 1,
                    "mumble": {
                        "processes_spawned": 1,
                        "reports_expected": 1,
                        "reports_received": 1,
                        "interrupted_processes": 0,
                        "missing_reports": 0,
                        "process_exit_failures": 0,
                        "clients_expected": 8,
                        "clients_reported": 8,
                        "clients_completed": 8,
                        "failure_rate": 0.0,
                        "maximum_worker_tcp_ping_p99_micros": 20_000,
                        "maximum_worker_udp_ping_p99_micros": 30_000,
                        "voice_packets_sent": 0,
                        "voice_packets_received": 0,
                    },
                    "driver_events": 100,
                    "credential_rotations": 8,
                    "ownership_losses": 0,
                    "ownership_violations": 0,
                    "stale_credential_rejections": 0,
                    "recovery_audits": 1,
                    "errors": 0,
                    "elapsed_millis": 2_500,
                }
            ),
            encoding="utf-8",
        )
        (run / "process-metrics.csv").write_text(
            "unix_millis,pid,role,rss_kib,cpu_percent\n"
            "1000,1,coordinator,10240,50\n"
            "1000,2,driver-load-controller-0,20480,20\n"
            "2000,1,coordinator,12288,75\n"
            "2000,2,driver-load-controller-0,24576,30\n",
            encoding="utf-8",
        )
        server_samples = [
            {
                "unix_millis": 1000,
                "actor": {
                    "commands": 0,
                    "responses": 0,
                    "participants": 0,
                    "spaces": 0,
                    "queue_depth": 0,
                    "queue_depth_max": 0,
                    "queue_saturations": 0,
                    "reconciliations": 0,
                    "reconciliation_max_nanos": 0,
                    "refused_publications": 0,
                    "lease_expirations": 0,
                },
                "connections": 0,
            },
            {
                "unix_millis": 2000,
                "actor": {
                    "commands": 40,
                    "responses": 80,
                    "participants": 8,
                    "spaces": 1,
                    "queue_depth": 2,
                    "queue_depth_max": 7,
                    "queue_saturations": 0,
                    "reconciliations": 8,
                    "reconciliation_max_nanos": 10_000_000,
                    "refused_publications": 0,
                    "lease_expirations": 0,
                },
                "connections": 8,
            },
        ]
        (run / "server-metrics.jsonl").write_text(
            "".join(json.dumps(sample) + "\n" for sample in server_samples), encoding="utf-8"
        )
        (run / "events.jsonl").write_text(
            json.dumps({"kind": "owned", "credential": "must-not-appear"}) + "\n",
            encoding="utf-8",
        )
        (run / "worker-0.jsonl").write_text(
            "".join(
                json.dumps(
                    {
                        "participant_id": f"participant-{index}",
                        "credential": "must-not-appear",
                        "event": {"kind": "synchronized", "monotonic_micros": micros},
                    }
                )
                + "\n"
                for index, micros in enumerate((100_000, 200_000, 300_000, 400_000))
            ),
            encoding="utf-8",
        )
        (run / "worker-0-report.json").write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "worker_index": 0,
                    "clients_expected": 8,
                    "stats": {
                        "clients_reported": 8,
                        "clients_completed": 8,
                        "tcp_ping_rtt": {"p99_micros": 20_000},
                        "udp_ping_rtt": {"p99_micros": 30_000},
                        "voice_packets_sent": 0,
                        "voice_packets_received": 0,
                    },
                }
            ),
            encoding="utf-8",
        )
        return run

    def test_generates_redacted_run_report_with_derived_metrics(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            run = self.make_run(Path(temporary))
            output, data = REPORT.write_run_report(run)

            self.assertEqual(data["workers"]["synchronization"]["p99"], 300.0)
            self.assertEqual(data["server"]["maxima"]["queue_depth_max"], 7)
            self.assertEqual(data["status"]["code"], "pass")
            self.assertEqual(data["workers"]["aggregate"]["clients_completed"], 8)
            document = output.read_text(encoding="utf-8")
            self.assertIn("Spaces headless load report", document)
            self.assertNotIn("must-not-appear", document)

    def test_incomplete_run_reports_known_os_failure_without_log_contents(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            run = Path(temporary) / "200-Idle-42"
            run.mkdir()
            (run / "manifest.json").write_text(
                json.dumps({"scenario": "idle", "participants": 128}), encoding="utf-8"
            )
            (run / "worker-0.log").write_text(
                "client 115: binding UDP: Too many open files (os error 24) SECRET\n",
                encoding="utf-8",
            )

            output, data = REPORT.write_run_report(run)

            self.assertEqual(data["status"]["code"], "incomplete")
            self.assertEqual(data["failures"][0]["code"], "file-descriptor-exhaustion")
            document = output.read_text(encoding="utf-8")
            self.assertIn("OS file descriptor limit reached", document)
            self.assertNotIn("SECRET", document)

    def test_result_root_generates_index_and_each_run_report(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            first = self.make_run(root, "100-Idle-1")
            second = self.make_run(root, "200-Idle-2")

            output, count = REPORT.write_campaign_report(root)

            self.assertEqual(count, 2)
            self.assertTrue(output.is_file())
            self.assertTrue((first / "report.html").is_file())
            self.assertTrue((second / "report.html").is_file())
            self.assertIn("100-Idle-1", output.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
