import json
import pathlib
import subprocess
import sys
import tempfile
import unittest

CONVERTER = pathlib.Path(__file__).with_name("soak-log-report.py")


def sample_line(label, transactions, rss=1000):
    counts = (
        "rx_bytes: 0, pending_requests: 0, pending_control: 0, pending_frames: 0, "
        "request_deadlines: 0, finished_acks: 10, displayed_timings: 10, "
        "held_frames: 0, diagnostics: 0, config_cache: 0, pending_configs: 0, "
        "active_pointers: 0, mirror_inbox_frames: 0, mirror_base_frames: 0, "
        "frame_transactions: 0, manager_events: 1, devices: 1"
    )
    return (
        f"R09 {label} Sample {{ rss_kib: Some({rss}), threads: Some(2), fds: Some(4), "
        f"counts: ContainerCounts {{ {counts} }}, transactions: {transactions} }}"
    )


def complete_log():
    return "\n".join(
        [
            "running 1 test",
            sample_line("baseline", 10),
            sample_line("midpoint", 50, 1001),
            sample_line("end", 100, 1002),
            "R09 RSS growth 2 KiB (budget 10240 KiB)",
            sample_line("idle", 100, 999),
            "test result: ok. 1 passed; 0 failed",
        ]
    )


class SoakLogReportTests(unittest.TestCase):
    def convert(self, text):
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "soak.log"
            output = pathlib.Path(directory) / "report.json"
            log.write_text(text)
            result = subprocess.run(
                [
                    sys.executable,
                    str(CONVERTER),
                    str(log),
                    "--commit",
                    "a" * 40,
                    "--output",
                    str(output),
                ],
                text=True,
                capture_output=True,
            )
            payload = json.loads(output.read_text()) if output.exists() else None
            return result, payload

    def test_complete_log_converts(self):
        result, payload = self.convert(complete_log())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(payload["commit"], "a" * 40)
        self.assertEqual(payload["rss_growth_kib"], 2)
        self.assertEqual(payload["samples"]["end"]["transactions"], 100)
        self.assertEqual(payload["samples"]["idle"]["rss_kib"], 999)

    def test_partial_log_is_rejected(self):
        lines = complete_log().splitlines()
        partial = "\n".join(line for line in lines if "R09 end" not in line)
        result, payload = self.convert(partial)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing samples", result.stderr)
        self.assertIsNone(payload)

    def test_failed_test_result_is_rejected(self):
        result, payload = self.convert(complete_log().replace("test result: ok.", "test result: FAILED."))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("did not pass", result.stderr)
        self.assertIsNone(payload)


if __name__ == "__main__":
    unittest.main()
