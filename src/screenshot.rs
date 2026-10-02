//! Monitor screenshots through the wlr-screencopy protocol, encoded as JPEG
//! for vision models.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use image::{DynamicImage, codecs::jpeg::JpegEncoder, imageops::FilterType};
use libwayshot::WayshotConnection;
use std::{process::Command, time::Instant};

const JPEG_QUALITY: u8 = 70;

#[derive(Debug, Clone)]
pub struct Monitor {
    pub name: String,
    pub description: String,
    /// Position and size in the compositor's global logical coordinates.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub struct Shot {
    pub monitor: Monitor,
    /// Size of the encoded image, which may be scaled down from the monitor.
    pub width: u32,
    pub height: u32,
    pub jpeg_base64: String,
}

impl Shot {
    pub fn data_url(&self) -> String {
        format!("data:image/jpeg;base64,{}", self.jpeg_base64)
    }

    pub fn describe(&self) -> String {
        format!(
            "Screenshot of monitor {} ({}), {}x{} image. Coordinates in computer actions are \
             pixels in this image, origin top-left.",
            self.monitor.name, self.monitor.description, self.width, self.height
        )
    }
}

pub fn monitors() -> Result<Vec<Monitor>> {
    let connection = WayshotConnection::new().context("could not connect to the compositor")?;
    Ok(connection
        .get_all_outputs()
        .iter()
        .map(|output| {
            let region = output.logical_region.inner;
            Monitor {
                name: output.name.clone(),
                description: output.description.clone(),
                x: region.position.x,
                y: region.position.y,
                width: region.size.width,
                height: region.size.height,
            }
        })
        .collect())
}

/// The output Niri has focused, if Niri is the compositor.
fn focused_output() -> Option<String> {
    let output = Command::new("niri")
        .args(["msg", "--json", "focused-output"])
        .output()
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    value.get("name")?.as_str().map(str::to_owned)
}

/// Captures the named monitor, or the focused one, at full resolution.
/// Blocking; run it off the async runtime.
pub fn capture_raw(name: Option<&str>) -> Result<(DynamicImage, Monitor)> {
    let connection = WayshotConnection::new().context("could not connect to the compositor")?;
    let outputs = connection.get_all_outputs();
    let output = match name {
        Some(name) => outputs
            .iter()
            .find(|output| output.name == name)
            .ok_or_else(|| {
                let names: Vec<&str> = outputs.iter().map(|output| output.name.as_str()).collect();
                anyhow!("no monitor named {name:?}; monitors: {}", names.join(", "))
            })?,
        None => {
            let focused = focused_output();
            outputs
                .iter()
                .find(|output| Some(&output.name) == focused.as_ref())
                .or(outputs.first())
                .context("the compositor reported no monitors")?
        }
    };
    let image = connection
        .screenshot_single_output(output, true)
        .with_context(|| format!("could not capture {}", output.name))?;
    let region = output.logical_region.inner;
    let monitor = Monitor {
        name: output.name.clone(),
        description: output.description.clone(),
        x: region.position.x,
        y: region.position.y,
        width: region.size.width,
        height: region.size.height,
    };
    Ok((image, monitor))
}

/// Scales an image down to fit `max_size` and encodes it as JPEG.
pub fn encode(image: &DynamicImage, monitor: Monitor, max_size: u32) -> Result<Shot> {
    let resized;
    let image = if image.width().max(image.height()) > max_size {
        resized = image.resize(max_size, max_size, FilterType::Triangle);
        &resized
    } else {
        image
    };
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, JPEG_QUALITY)
        .encode_image(&image.to_rgb8())
        .context("could not encode the screenshot")?;
    Ok(Shot {
        monitor,
        width: image.width(),
        height: image.height(),
        jpeg_base64: base64::engine::general_purpose::STANDARD.encode(&jpeg),
    })
}

pub fn capture(name: Option<&str>, max_size: u32) -> Result<Shot> {
    let started = Instant::now();
    let (image, monitor) = capture_raw(name)?;
    let shot = encode(&image, monitor, max_size)?;
    log!(
        "INFO",
        "screenshot_taken",
        "monitor={} size={}x{} base64_bytes={} duration_ms={}",
        shot.monitor.name,
        shot.width,
        shot.height,
        shot.jpeg_base64.len(),
        started.elapsed().as_millis()
    );
    Ok(shot)
}
