// SPDX-License-Identifier: MIT
//! Regions use the displayed, upright source's normalized coordinates. Cropping
//! occurs before downsampling and never relaxes decoder/source memory limits.
use crate::{MediaError, Result, pictures::OutputFormat};
use serde::{Deserialize, Serialize};
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct PixelRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
impl Region {
    pub fn validate(&self) -> Result<()> {
        if [self.x, self.y, self.width, self.height]
            .iter()
            .any(|x| !x.is_finite())
            || self.x < 0.
            || self.y < 0.
            || self.width <= 0.
            || self.height <= 0.
            || self.x + self.width > 1.000000001
            || self.y + self.height > 1.000000001
        {
            return Err(MediaError::Invalid(
                "region must lie within normalized source coordinates 0..1".into(),
            ));
        }
        Ok(())
    }
    pub fn pixels(&self, width: u32, height: u32) -> Result<PixelRegion> {
        self.validate()?;
        if width == 0 || height == 0 {
            return Err(MediaError::Invalid("empty region source".into()));
        }
        let x = (self.x * f64::from(width)).floor() as u32;
        let y = (self.y * f64::from(height)).floor() as u32;
        let end_x = ((self.x + self.width) * f64::from(width))
            .ceil()
            .min(f64::from(width)) as u32;
        let end_y = ((self.y + self.height) * f64::from(height))
            .ceil()
            .min(f64::from(height)) as u32;
        if end_x <= x || end_y <= y {
            return Err(MediaError::Invalid("empty pixel region".into()));
        }
        Ok(PixelRegion {
            x,
            y,
            width: end_x - x,
            height: end_y - y,
        })
    }
}
#[derive(Clone, Copy, Debug)]
pub struct ViewOptions {
    pub region: Option<Region>,
    pub format: OutputFormat,
}
impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            region: None,
            format: OutputFormat::Jpeg,
        }
    }
}
impl ViewOptions {
    pub fn pixels(&self, width: u32, height: u32) -> Result<PixelRegion> {
        self.region
            .map(|r| r.pixels(width, height))
            .unwrap_or(Ok(PixelRegion {
                x: 0,
                y: 0,
                width,
                height,
            }))
    }
}
