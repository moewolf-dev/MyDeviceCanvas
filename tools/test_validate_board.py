import unittest
from pathlib import Path

from validate_board import validate


ROOT = Path(__file__).parent.parent


class BoardValidationTests(unittest.TestCase):
    def test_unverified_candidate_is_rejected(self):
        errors = validate(ROOT / "boards/esp32/jc3248w535/board.yaml")
        self.assertTrue(any("unknown" in error for error in errors))

    def test_complete_profile_is_accepted(self):
        self.assertEqual(validate(ROOT / "boards/esp32/test-valid/board.yaml"), [])


if __name__ == "__main__":
    unittest.main()
