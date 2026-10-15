//! Sprite-atlas baker (N11). Turns motion metadata into a deterministic frame
//! list and atlas grid. This is a *plan*: no pixels are decoded or packed, so a
//! weak MCU target knows exactly how many frames and how large an atlas it will
//! receive before provisioning. Bounds are checked before any allocation.
use crate::ir::ModelError;
use crate::live2d::MotionInfo;
use serde::{Deserialize, Serialize};

/// Frames a single motion contributes and where they sit in the atlas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MotionFrames {
    pub name: String,
    pub start: u32,
    pub count: u32,
}

/// A deterministic sprite-atlas layout for one animation set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasPlan {
    pub sprite_width: u32,
    pub sprite_height: u32,
    pub frames: u32,
    pub columns: u32,
    pub rows: u32,
    /// frames × sprite_w × sprite_h × 2 bytes (RGB565).
    pub total_bytes: u64,
    pub motions: Vec<MotionFrames>,
}

/// Baker limits. Defaults are intermediate-buffer limits, not product promises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtlasBaker {
    pub max_frames: u32,
    pub max_atlas_bytes: u64,
}
impl Default for AtlasBaker {
    fn default() -> Self {
        Self {
            max_frames: 4096,
            max_atlas_bytes: 32 * 1024 * 1024,
        }
    }
}

impl AtlasBaker {
    /// Sample every motion at its own fps for one animation cycle and lay the
    /// frames on a near-square grid bounded by the device texture limit.
    pub fn bake(
        &self,
        sprite: (u32, u32),
        max_texture: u32,
        motions: &[MotionInfo],
    ) -> Result<AtlasPlan, ModelError> {
        let sprite_width = sprite.0.min(max_texture);
        let sprite_height = sprite.1.min(max_texture);
        if sprite_width == 0 || sprite_height == 0 {
            return Err(ModelError::Unsupported(
                "sprite size is unknown (0); decode the atlas before baking".into(),
            ));
        }
        if motions.is_empty() {
            return Err(ModelError::Unsupported("no motions to bake".into()));
        }

        let mut frames = Vec::with_capacity(motions.len());
        let mut total: u32 = 0;
        for motion in motions {
            let count = motion.frame_count();
            if count == 0 {
                return Err(ModelError::Unsupported(format!(
                    "motion {} has no frames (duration/fps)",
                    motion.name
                )));
            }
            let start = total;
            total = total
                .checked_add(count)
                .ok_or_else(|| ModelError::Budget("frame total overflow".into()))?;
            frames.push(MotionFrames {
                name: motion.name.clone(),
                start,
                count,
            });
        }
        if total > self.max_frames {
            return Err(ModelError::Budget(format!(
                "{total} frames over {} limit",
                self.max_frames
            )));
        }

        // Near-square grid, deterministic and independent of insertion order.
        let columns = (f64::from(total).sqrt().ceil()).max(1.0) as u32;
        let rows = total.div_ceil(columns);
        let atlas_w = columns.saturating_mul(sprite_width);
        let atlas_h = rows.saturating_mul(sprite_height);
        if atlas_w > max_texture || atlas_h > max_texture {
            return Err(ModelError::Budget(format!(
                "atlas {atlas_w}×{atlas_h} over texture limit {max_texture}"
            )));
        }
        let total_bytes = u64::from(total)
            .saturating_mul(u64::from(sprite_width))
            .saturating_mul(u64::from(sprite_height))
            .saturating_mul(2);
        if total_bytes > self.max_atlas_bytes {
            return Err(ModelError::Budget(format!(
                "atlas {total_bytes} bytes over {} budget",
                self.max_atlas_bytes
            )));
        }

        Ok(AtlasPlan {
            sprite_width,
            sprite_height,
            frames: total,
            columns,
            rows,
            total_bytes,
            motions: frames,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(name: &str, duration: f64, fps: f64) -> MotionInfo {
        MotionInfo {
            name: name.into(),
            channels: 2,
            duration_sec: duration,
            fps,
            fade_in_sec: 0.0,
            fade_out_sec: 0.0,
            looping: true,
            curves: Vec::new(),
        }
    }

    #[test]
    fn bakes_a_deterministic_grid() {
        let baker = AtlasBaker::default();
        let plan = baker
            .bake(
                (64, 64),
                2048,
                &[motion("idle", 2.0, 30.0), motion("tap", 1.0, 30.0)],
            )
            .unwrap();
        // 61 + 31 = 92 frames → 10×10 grid.
        assert_eq!(plan.frames, 92);
        assert_eq!(plan.columns, 10);
        assert_eq!(plan.rows, 10);
        assert_eq!(
            plan.motions[0],
            MotionFrames {
                name: "idle".into(),
                start: 0,
                count: 61
            }
        );
        assert_eq!(
            plan.motions[1],
            MotionFrames {
                name: "tap".into(),
                start: 61,
                count: 31
            }
        );
        assert_eq!(plan.total_bytes, 92 * 64 * 64 * 2);
    }

    #[test]
    fn sprite_is_clamped_before_the_grid_check() {
        // A 4096² sprite is clamped to the 256 texture limit; the 2×2 grid of
        // 256² sprites then reports the clamped 512×512 atlas, so the rejection
        // proves the clamp ran first.
        let err = AtlasBaker::default()
            .bake((4096, 4096), 256, &[motion("idle", 0.1, 30.0)])
            .unwrap_err();
        match err {
            ModelError::Budget(message) => assert!(message.contains("512×512"), "{message}"),
            other => panic!("expected a budget error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_sprite_size_and_empty_motions_are_rejected() {
        assert!(matches!(
            AtlasBaker::default().bake((0, 0), 2048, &[motion("idle", 1.0, 30.0)]),
            Err(ModelError::Unsupported(_))
        ));
        assert!(matches!(
            AtlasBaker::default().bake((64, 64), 2048, &[]),
            Err(ModelError::Unsupported(_))
        ));
    }

    #[test]
    fn frame_and_byte_budgets_are_enforced() {
        let baker = AtlasBaker {
            max_frames: 10,
            ..AtlasBaker::default()
        };
        assert!(matches!(
            baker.bake((64, 64), 4096, &[motion("idle", 2.0, 30.0)]),
            Err(ModelError::Budget(_))
        ));

        let tiny = AtlasBaker {
            max_atlas_bytes: 1024,
            ..AtlasBaker::default()
        };
        assert!(matches!(
            tiny.bake((64, 64), 4096, &[motion("idle", 2.0, 30.0)]),
            Err(ModelError::Budget(_))
        ));
    }

    #[test]
    fn atlas_larger_than_the_texture_limit_is_rejected() {
        // 61 frames of 512×512 on a 512-texture device needs a multi-sprite grid
        // wider than the limit.
        let err = AtlasBaker::default()
            .bake((512, 512), 512, &[motion("idle", 2.0, 30.0)])
            .unwrap_err();
        assert!(matches!(err, ModelError::Budget(_)), "{err:?}");
    }
}
