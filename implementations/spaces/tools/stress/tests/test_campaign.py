import base64
import importlib.util
import io
import json
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path


CAMPAIGN_PATH = Path(__file__).parents[1] / "campaign.py"
SPEC = importlib.util.spec_from_file_location("spaces_stress_campaign", CAMPAIGN_PATH)
if SPEC is None or SPEC.loader is None:
    raise RuntimeError("could not load campaign.py")
CAMPAIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAMPAIGN)


def sample_matrix() -> dict:
    points = []
    for per_space in (8, 32, 64, 128, 256):
        for participants in (per_space, 3_200):
            if participants % per_space:
                continue
            for audio in ("none", "one_per_space", "five_percent"):
                points.append(
                    {
                        "participants_per_space": per_space,
                        "participants": participants,
                        "spaces": participants // per_space,
                        "audio": audio,
                    }
                )
    return {"schema_version": 1, "points": points}


class CampaignTest(unittest.TestCase):
    def test_quick_profile_selects_one_silent_full_space_per_cardinality(self) -> None:
        points = CAMPAIGN.plan_points(
            sample_matrix(),
            "quick",
            (8, 32, 64, 128, 256),
            4_096,
            (42,),
            "auto",
            100,
            8,
        )

        self.assertEqual(len(points), 5)
        self.assertTrue(all(point["participants"] == point["participants_per_space"] for point in points))
        self.assertTrue(all(point["audio"] == "none" for point in points))

    def test_capacity_profile_uses_the_official_silent_points_and_all_seeds(self) -> None:
        points = CAMPAIGN.plan_points(
            sample_matrix(), "capacity", (32,), 3_200, (41, 42), "auto", 100, 8
        )

        self.assertEqual([(point["participants"], point["seed"]) for point in points], [
            (32, 41),
            (32, 42),
            (3_200, 41),
            (3_200, 42),
        ])
        self.assertEqual(points[-1]["controllers"], 32)
        self.assertEqual(points[-1]["driver_processes"], 4)

    def test_fixed_controller_count_rejects_per_controller_target_overflow(self) -> None:
        with self.assertRaisesRegex(ValueError, "use at least 32"):
            CAMPAIGN.controller_count(3_200, 4, 100)

    def test_auto_controller_count_scales_beyond_the_power_of_two_range(self) -> None:
        self.assertEqual(CAMPAIGN.controller_count(3_200, "auto", 100), 32)
        self.assertEqual(CAMPAIGN.controller_count(10_000, "auto", 100), 100)

    def test_audio_command_uses_exact_matrix_level_and_voice_fixture(self) -> None:
        command = CAMPAIGN.command_for_point(
            {
                "controllers": 2,
                "driver_processes": 1,
                "controllers_per_driver_process": 8,
                "max_participants_per_controller": 100,
                "participants": 64,
                "participants_per_space": 32,
                "seed": 42,
                "audio": "one_per_space",
            },
            Path("stress"),
            Path("driver"),
            Path("runs"),
            Path("voice.opuspack"),
            "20s",
            "10s",
        )

        self.assertEqual(command[command.index("--scenario") + 1], "voice")
        self.assertEqual(command[command.index("--audio") + 1], "one-per-space")
        self.assertEqual(command[command.index("--voice-file") + 1], "voice.opuspack")
        self.assertEqual(command[command.index("--max-participants-per-controller") + 1], "100")
        self.assertEqual(command[command.index("--controllers-per-driver-process") + 1], "8")

    def test_voice_fixture_accepts_wrapped_base64(self) -> None:
        packet = bytes(range(30))
        with tempfile.TemporaryDirectory() as temporary:
            campaign = Path(temporary)
            fixture = campaign / "voice.opuspack.base64"
            fixture.write_bytes(base64.b64encode(packet) + b"\n")

            destination = CAMPAIGN.prepare_voice_file(campaign, fixture)

            self.assertEqual(destination.read_bytes(), packet)

    def test_load_campaign_turns_abandoned_running_point_into_interrupted(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            campaign = Path(temporary)
            state = {
                "schema_version": 1,
                "updated_at": "old",
                "configuration": {},
                "points": [{"id": "point", "status": "running"}],
            }
            (campaign / "campaign-state.json").write_text(json.dumps(state), encoding="utf-8")

            loaded = CAMPAIGN.load_campaign(campaign)

            self.assertEqual(loaded["points"][0]["status"], "interrupted")
            persisted = json.loads((campaign / "campaign-state.json").read_text(encoding="utf-8"))
            self.assertEqual(persisted["points"][0]["status"], "interrupted")

    def test_plan_command_is_side_effect_free_with_an_explicit_matrix(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            matrix = Path(temporary) / "matrix.json"
            matrix.write_text(json.dumps(sample_matrix()), encoding="utf-8")
            output = io.StringIO()

            with redirect_stdout(output):
                result = CAMPAIGN.main(
                    [
                        "plan",
                        "--profile",
                        "quick",
                        "--matrix-file",
                        str(matrix),
                    ]
                )

            self.assertEqual(result, 0)
            self.assertIn("5 benchmark run(s)", output.getvalue())


if __name__ == "__main__":
    unittest.main()
