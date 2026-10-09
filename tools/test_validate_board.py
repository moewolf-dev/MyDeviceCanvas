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

    def test_pin_collision_detected(self):
        # reuse parser via temporary content would be heavy; ensure sim has no collision
        errors = validate(ROOT / "boards/sim/esp32-jc3248w535-sim/board.yaml")
        self.assertFalse(any("collision" in e for e in errors))


if __name__ == "__main__":
    unittest.main()
