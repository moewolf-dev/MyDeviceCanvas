//! Host-side board facts for firmware preflight (CORE-R06/CORE-R02 follow-up).
//!
//! The profile data lives in `boards/**/board.yaml`; this table is the runtime
//! copy the SDK can use without parsing YAML in a packaged Electron app. A unit
//! test re-reads every YAML and fails when the table drifts.
use crate::{BoardFacts, FlashSize};

#[derive(Debug, Clone, Copy)]
pub struct KnownBoard {
    pub board_id: &'static str,
    pub mcu: &'static str,
    pub flash_bytes: u64,
    /// True only for simulator baselines that the host may preflight without a
    /// physical measurement. Reference/unverified and Linux boards stay false.
    pub profile_verified: bool,
}

/// Boards shipped in `boards/`. Keep in sync with the YAML files; the test in
/// this module verifies id/mcu/flash for each entry.
pub const KNOWN_BOARDS: &[KnownBoard] = &[
    KnownBoard {
        board_id: "esp32-jc3248w535-sim",
        mcu: "esp32-s3",
        flash_bytes: 16 * 1024 * 1024,
        profile_verified: true,
    },
    KnownBoard {
        board_id: "linux-virt",
        mcu: "linux-host",
        flash_bytes: 1,
        // A Linux runtime is not a serial-flash target for this SDK.
        profile_verified: false,
    },
    KnownBoard {
        board_id: "esp32s3-box-3",
        mcu: "esp32-s3",
        flash_bytes: 16 * 1024 * 1024,
        profile_verified: true,
    },
    KnownBoard {
        board_id: "jc3248w535",
        mcu: "esp32-s3",
        flash_bytes: 16 * 1024 * 1024,
        // Reference-unverified: physical flash/probe pending (D05).
        profile_verified: false,
    },
    KnownBoard {
        board_id: "test-valid",
        mcu: "esp32-s3",
        flash_bytes: 16 * 1024 * 1024,
        profile_verified: false,
    },
    KnownBoard {
        board_id: "pi4-hdmi",
        mcu: "bcm2711",
        flash_bytes: 1,
        profile_verified: false,
    },
    KnownBoard {
        board_id: "waveshare-eink",
        mcu: "bcm2711",
        flash_bytes: 1,
        profile_verified: false,
    },
    KnownBoard {
        board_id: "jetson-orin-nano",
        mcu: "tegra234",
        flash_bytes: 8 * 1024 * 1024,
        profile_verified: false,
    },
    KnownBoard {
        board_id: "rk3399-nanopc-t4",
        mcu: "rk3399",
        flash_bytes: 4 * 1024 * 1024,
        profile_verified: false,
    },
    KnownBoard {
        board_id: "arduino-uno-r4-ili9341",
        mcu: "ra4m1",
        flash_bytes: 256 * 1024,
        profile_verified: false,
    },
    KnownBoard {
        board_id: "arduino-uno-tft",
        mcu: "atmega328p",
        flash_bytes: 32 * 1024,
        profile_verified: false,
    },
];

/// Return the host-side facts for a known board id. Unknown ids are reported as
/// unknown rather than guessed from the first profile.
pub fn known_board_facts(board_id: &str) -> Option<BoardFacts> {
    KNOWN_BOARDS
        .iter()
        .find(|board| board.board_id == board_id)
        .map(|board| BoardFacts {
            board_id: board.board_id.to_string(),
            mcu: board.mcu.to_string(),
            flash: FlashSize::Known(board.flash_bytes),
            profile_verified: board.profile_verified,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    fn board_yaml_files() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../boards");
        let mut out = Vec::new();
        for family in std::fs::read_dir(root).unwrap() {
            let family = family.unwrap().path();
            if !family.is_dir() {
                continue;
            }
            for board in std::fs::read_dir(family).unwrap() {
                let board = board.unwrap().path();
                let yaml = board.join("board.yaml");
                if yaml.is_file() {
                    out.push(yaml);
                }
            }
        }
        out
    }

    fn scalar(text: &str, key: &str) -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix(&format!("{key}:")))
            .map(|value| value.trim().to_string())
    }

    #[test]
    fn table_matches_every_board_yaml() {
        let mut from_disk: BTreeMap<String, (String, u64)> = BTreeMap::new();
        for path in board_yaml_files() {
            let text = std::fs::read_to_string(&path).unwrap();
            let id = scalar(&text, "id").unwrap_or_else(|| panic!("id missing: {path:?}"));
            let mcu = scalar(&text, "mcu").unwrap_or_else(|| panic!("mcu missing: {path:?}"));
            let flash: u64 = scalar(&text, "flash_bytes")
                .unwrap_or_else(|| panic!("flash_bytes missing: {path:?}"))
                .parse()
                .unwrap();
            assert!(
                from_disk.insert(id.clone(), (mcu, flash)).is_none(),
                "duplicate board id {id}"
            );
        }
        let table: BTreeMap<String, (String, u64)> = KNOWN_BOARDS
            .iter()
            .map(|board| {
                (
                    board.board_id.to_string(),
                    (board.mcu.to_string(), board.flash_bytes),
                )
            })
            .collect();
        assert_eq!(
            table, from_disk,
            "KNOWN_BOARDS drifted from boards/*/*/board.yaml"
        );
    }

    #[test]
    fn unknown_boards_are_not_guessed() {
        assert!(known_board_facts("not-a-board").is_none());
        let sim = known_board_facts("esp32-jc3248w535-sim").unwrap();
        assert_eq!(sim.mcu, "esp32-s3");
        assert_eq!(sim.flash, FlashSize::Known(16 * 1024 * 1024));
        assert!(sim.profile_verified);
        let reference = known_board_facts("jc3248w535").unwrap();
        assert!(!reference.profile_verified);
    }
}
