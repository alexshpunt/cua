//! Lossless readonly crops of original WGC pixels; no snapshot or input context.

use super::persistent_wgc::{Frame, Target};
use image::{DynamicImage, ImageDecoder, ImageFormat};
use serde_json::{json, Value};
use std::io::Cursor;

const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_PNG_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

fn coordinate(value: &Value) -> Option<i32> {
    let value = value.as_f64()?;
    (value.is_finite()
        && value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX))
    .then_some(value as i32)
}

fn bounded(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

impl Region {
    /// Admit exact screen-physical edges without rounding, clipping or scale fitting.
    pub(crate) fn from_args(args: &Value) -> Result<Self, &'static str> {
        let parse = || {
            let (x, y, right, bottom) = (
                coordinate(args.get("x1")?)?,
                coordinate(args.get("y1")?)?,
                coordinate(args.get("x2")?)?,
                coordinate(args.get("y2")?)?,
            );
            let width = u32::try_from(right.checked_sub(x)?).ok()?;
            let height = u32::try_from(bottom.checked_sub(y)?).ok()?;
            bounded(width, height).then_some(Self {
                x,
                y,
                width,
                height,
            })
        };
        parse().ok_or("wgc_crop_invalid_region")
    }
}

/// Require an explicit native target and the dispatch-owned runtime session.
pub(crate) fn request(args: &Value) -> Result<(Target, Region), &'static str> {
    let pid = args
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0 && *pid <= i32::MAX as u32)
        .ok_or("wgc_crop_explicit_pid_required")?;
    let window = args
        .get("window_id")
        .and_then(Value::as_u64)
        .filter(|window| *window > 0)
        .ok_or("wgc_crop_explicit_window_required")?;
    let session = args
        .get("_session_id")
        .and_then(Value::as_str)
        .filter(|session| !session.is_empty())
        .ok_or("wgc_runtime_session_required")?;
    Ok((
        Target {
            session: session.to_owned(),
            pid,
            window,
        },
        Region::from_args(args)?,
    ))
}
/// Crop only a proven one-to-one WGC frame. The worker checks cancellation and
/// native lifetime again before publishing these pixels; this grants no input.
pub(crate) fn crop(frame: Frame, region: Region, target: &Target) -> Result<Frame, &'static str> {
    let metadata = &frame.metadata;
    let bounds = metadata
        .get("frame_bounds")
        .and_then(Value::as_object)
        .ok_or("wgc_crop_frame_unmapped")?;
    if metadata["backend"] != "wgc"
        || metadata["paint_freshness"] != "unverified"
        || metadata
            .get("scale")
            .is_some_and(|scale| scale.as_f64() != Some(1.0))
        || bounds.len() != 4
        || bounds.get("width").and_then(Value::as_u64) != Some(u64::from(frame.width))
        || bounds.get("height").and_then(Value::as_u64) != Some(u64::from(frame.height))
        || !bounded(frame.width, frame.height)
    {
        return Err("wgc_crop_frame_unmapped");
    }
    let (x, y) = (
        bounds
            .get("x")
            .and_then(coordinate)
            .ok_or("wgc_crop_frame_unmapped")?,
        bounds
            .get("y")
            .and_then(coordinate)
            .ok_or("wgc_crop_frame_unmapped")?,
    );
    if x.checked_add(frame.width as i32).is_none() || y.checked_add(frame.height as i32).is_none() {
        return Err("wgc_crop_frame_unmapped");
    }
    let (offset_x, offset_y) = (
        i64::from(region.x) - i64::from(x),
        i64::from(region.y) - i64::from(y),
    );
    if !bounded(region.width, region.height)
        || offset_x < 0
        || offset_y < 0
        || offset_x + i64::from(region.width) > i64::from(frame.width)
        || offset_y + i64::from(region.height) > i64::from(frame.height)
    {
        return Err("wgc_crop_region_outside_frame");
    }
    if frame.png.len() > MAX_PNG_BYTES {
        return Err("wgc_crop_invalid_png");
    }
    let decoder = image::codecs::png::PngDecoder::new(Cursor::new(&frame.png))
        .map_err(|_| "wgc_crop_invalid_png")?;
    if decoder.dimensions() != (frame.width, frame.height) || decoder.total_bytes() > MAX_PIXELS * 4
    {
        return Err("wgc_crop_frame_unmapped");
    }
    let image = DynamicImage::from_decoder(decoder).map_err(|_| "wgc_crop_invalid_png")?;
    let cropped = image.crop_imm(
        offset_x as u32,
        offset_y as u32,
        region.width,
        region.height,
    );
    let mut png = Cursor::new(Vec::new());
    cropped
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|_| "wgc_crop_png_encoding_failed")?;
    let metadata = json!({
        "kind":"wgc_crop_v1", "pid":target.pid, "window_id":target.window,
        "coordinate_space":"screen_physical", "frame_bounds":bounds,
        "source_dimensions":{"width":frame.width,"height":frame.height},
        "crop_bounds":{"x":region.x,"y":region.y,"width":region.width,"height":region.height},
        "crop_offset":{"x":offset_x,"y":offset_y},
        "width":region.width,"height":region.height,"format":"png","mime_type":"image/png",
        "scale":1,"padding":0,"input_authority":false,"paint_freshness":"unverified",
        "capture_metadata":frame.metadata
    });
    Ok(Frame {
        png: png.into_inner(),
        width: region.width,
        height: region.height,
        metadata,
    })
}
