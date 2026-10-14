//! Offline qualification of two original PNG samples, not a browser/input tool.
//! Usage: cargo run -p cua-driver-core --example dom_pixel_proof -- native.png page.png
//! No resampling, tolerance, guessed inset or native input authority is supplied.

use cua_driver_core::browser::dom_pixel_match::match_viewport;
use cua_driver_core::image_utils::png_dimensions;
use image::{ImageFormat, RgbImage};
use serde_json::json;
use std::{fs, path::Path};

fn decode(path: &Path) -> anyhow::Result<RgbImage> {
    anyhow::ensure!(fs::metadata(path)?.len() <= 16_777_216, "encoded PNG limit");
    let bytes = fs::read(path)?;
    let (width, height) = png_dimensions(&bytes)?;
    anyhow::ensure!(width > 0 && height > 0 && width <= 16_384 && height <= 16_384 &&
        u64::from(width) * u64::from(height) <= 16_777_216, "decoded PNG limit");
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)?;
    anyhow::ensure!(image.width() == width && image.height() == height, "PNG dimensions changed");
    anyhow::ensure!(image.to_rgba8().pixels().all(|pixel| pixel[3] == 255), "unproven transparent pixels");
    Ok(image.to_rgb8())
}

fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(args.len() == 2, "usage: dom_pixel_proof native.png page.png");
    let native = decode(Path::new(&args[0]))?;
    let page = decode(Path::new(&args[1]))?;
    let result = match_viewport(&native, &page);
    println!("{}", match result {
        Ok(proof) => json!({"status":"matched", "method":"unique_exact_rgb", "x":proof.x, "y":proof.y,
            "width":proof.width, "height":proof.height, "native_dimensions":native.dimensions(),
            "page_dimensions":page.dimensions(), "read_only":true, "input_authority":false,
            "atomic_capture":false}),
        Err(code) => json!({"status":"refused", "code":code, "native_dimensions":native.dimensions(),
            "page_dimensions":page.dimensions(), "read_only":true, "input_authority":false}),
    });
    Ok(())
}
