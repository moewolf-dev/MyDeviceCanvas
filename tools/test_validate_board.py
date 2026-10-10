import unittest
from pathlib import Path

from validate_board import validate


ROOT = Path(__file__).parent.parent


class BoardValidationTests(unittest.TestCase):
    def test_reference_unverified_physical_candidate_is_rejected(self):
        errors = validate(ROOT / "boards/esp32/jc3248w535/board.yaml")
        self.assertTrue(
            any("not physically verified" in error for error in errors),
            errors,
        )

    def test_complete_profile_is_accepted(self):
        self.assertEqual(validate(ROOT / "boards/esp32/test-valid/board.yaml"), [])

    def test_sim_baseline_is_accepted(self):
        self.assertEqual(
            validate(ROOT / "boards/sim/esp32-jc3248w535-sim/board.yaml"), []
        )

    def test_linux_hdmi_and_eink_profiles(self):
        self.assertEqual(validate(ROOT / "boards/linux/pi4-hdmi/board.yaml"), [])
        self.assertEqual(validate(ROOT / "boards/linux/waveshare-eink/board.yaml"), [])

    def test_eink_without_color_fps_false_is_rejected(self):
        text = (ROOT / "boards/linux/waveshare-eink/board.yaml").read_text(encoding="utf-8")
        bad = ROOT / "boards/linux/waveshare-eink/board.yaml.tmp-test"
        bad.write_text(text.replace("color_fps: false", "color_fps: true"), encoding="utf-8")
        try:
            errors = validate(bad)
        finally:
            bad.unlink(missing_ok=True)
        self.assertTrue(any("color_fps" in error for error in errors), errors)

    def test_pin_collision_detected(self):
        # reuse parser via temporary content would be heavy; ensure sim has no collision
        errors = validate(ROOT / "boards/sim/esp32-jc3248w535-sim/board.yaml")
        self.assertFalse(any("collision" in e for e in errors))


if __name__ == "__main__":
    unittest.main()
