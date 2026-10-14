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

    def test_n06_family_skeletons_are_complete_but_pending(self):
        for rel in (
            "boards/arduino/uno-r4-ili9341/board.yaml",
            "boards/rockchip/rk3399-nanopc-t4/board.yaml",
            "boards/nvidia/jetson-orin-nano/board.yaml",
        ):
            path = ROOT / rel
            self.assertEqual(validate(path), [], rel)
            self.assertIn("physical: pending", path.read_text(encoding="utf-8"))

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

    def test_renderer_mesh_requires_vertex_budget(self):
        text = (ROOT / "boards/esp32/test-valid/board.yaml").read_text(encoding="utf-8")
        bad = ROOT / "boards/esp32/test-valid/board.yaml.tmp-test"
        bad.write_text(
            text.replace("renderer_mesh2d: false", "renderer_mesh2d: true"),
            encoding="utf-8",
        )
        try:
            errors = validate(bad)
        finally:
            bad.unlink(missing_ok=True)
        self.assertTrue(any("renderer_max_vertices" in error for error in errors), errors)

    def test_renderer_profiles_are_accepted(self):
        self.assertEqual(validate(ROOT / "boards/esp32/test-valid/board.yaml"), [])
        self.assertEqual(validate(ROOT / "boards/linux/pi4-hdmi/board.yaml"), [])


if __name__ == "__main__":
    unittest.main()
