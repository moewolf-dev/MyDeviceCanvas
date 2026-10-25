import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

VALIDATOR = pathlib.Path(__file__).with_name("validate-long-report.py")


def sample(transactions=100):
    return {
        "rss_kib": 1000,
        "threads": 2,
        "fds": 4,
        "transactions": transactions,
        "containers": {
            "pending_requests": 0,
            "held_frames": 0,
            "frame_transactions": 0,
        },
    }


def report():
    return {
        "mode": "long",
        "commit": "a" * 40,
        "ok": True,
        "config": {
            "warmup_seconds": 300,
            "steady_seconds": 3600,
            "idle_seconds": 60,
            "rss_budget_kib": 10240,
        },
        "samples": {
            "baseline": sample(10),
            "midpoint": sample(50),
            "end": sample(100),
            "idle": sample(100),
        },
        "rss_growth_kib": 128,
        "steps": [{"name": "long-soak", "status": "passed"}],
    }


class ValidateLongReportTests(unittest.TestCase):
    def run_validator(self, payload, *args):
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump(payload, handle)
            path = handle.name
        try:
            return subprocess.run(
                [sys.executable, str(VALIDATOR), path, *args],
                text=True,
                capture_output=True,
            )
        finally:
            pathlib.Path(path).unlink(missing_ok=True)

    def test_valid_report_passes(self):
        result = self.run_validator(report(), "--require-commit", "a" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_short_steady_window_fails(self):
        payload = report()
        payload["config"]["steady_seconds"] = 10
        result = self.run_validator(payload)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("steady_seconds", result.stderr)

    def test_rss_over_budget_fails(self):
        payload = report()
        payload["rss_growth_kib"] = 10241
        result = self.run_validator(payload)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RSS growth", result.stderr)

    def test_missing_sample_fails(self):
        payload = report()
        del payload["samples"]["midpoint"]
        result = self.run_validator(payload)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("midpoint", result.stderr)

    def test_commit_mismatch_fails(self):
        result = self.run_validator(report(), "--require-commit", "b" * 40)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("commit", result.stderr)


if __name__ == "__main__":
    unittest.main()
