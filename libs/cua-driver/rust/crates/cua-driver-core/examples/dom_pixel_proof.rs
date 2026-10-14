//! Offline qualification of two original PNG samples, not a browser/input tool.
//! Usage: cargo run -p cua-driver-core --example dom_pixel_proof -- native.png page.png
//! No resampling, tolerance, guessed inset or native input authority is supplied.

use cua_driver_core::browser::dom_pixel_match::{match_viewport, PixelPlacement};
use cua_driver_core::image_utils::png_dimensions;
use image::{DynamicImage, GenericImageView, ImageFormat};
use serde_json::json;
use std::{fs, path::Path};

fn decode(path: &Path) -> anyhow::Result<DynamicImage> {
    anyhow::ensure!(fs::metadata(path)?.len() <= 16_777_216, "encoded PNG limit");
    let bytes = fs::read(path)?;
    let (width, height) = png_dimensions(&bytes)?;
    anyhow::ensure!(
        width > 0
            && height > 0
            && width <= 16_384
            && height <= 16_384
            && u64::from(width) * u64::from(height) <= 16_777_216,
        "decoded PNG limit"
    );
    let image = image::load_from_memory_with_format(&bytes, ImageFormat::Png)?;
    anyhow::ensure!(
        image.width() == width && image.height() == height,
        "PNG dimensions changed"
    );
    Ok(image)
}

fn match_samples(
    native: &DynamicImage,
    page: &DynamicImage,
) -> Result<PixelPlacement, &'static str> {
    if !page.pixels().all(|(_, _, pixel)| pixel[3] == 255) {
        return Err("dom_pixel_page_alpha_unproven");
    }
    let proof = match_viewport(&native.to_rgb8(), &page.to_rgb8())?;
    if !native
        .view(proof.x, proof.y, proof.width, proof.height)
        .pixels()
        .all(|(_, _, pixel)| pixel[3] == 255)
    {
        return Err("dom_pixel_native_alpha_unproven");
    }
    Ok(proof)
}
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    anyhow::ensure!(
        args.len() == 2,
        "usage: dom_pixel_proof native.png page.png"
    );
    let native = decode(Path::new(&args[0]))?;
    let page = decode(Path::new(&args[1]))?;
    let result = match_samples(&native, &page);
    println!(
        "{}",
        match result {
            Ok(proof) =>
                json!({"status":"matched", "method":"unique_exact_rgb", "x":proof.x, "y":proof.y,
            "width":proof.width, "height":proof.height, "native_dimensions":native.dimensions(),
            "page_dimensions":page.dimensions(), "read_only":true, "input_authority":false,
            "atomic_capture":false}),
            Err(code) =>
                json!({"status":"refused", "code":code, "native_dimensions":native.dimensions(),
            "page_dimensions":page.dimensions(), "read_only":true, "input_authority":false}),
        }
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn samples() -> (DynamicImage, DynamicImage) {
        let page = RgbaImage::from_fn(2, 2, |x, y| {
            Rgba([21 + x as u8 * 30, 30 + y as u8 * 40, 99, 255])
        });
        let mut native = RgbaImage::from_pixel(7, 5, Rgba([4, 8, 9, 255]));
        for (x, y, pixel) in page.enumerate_pixels() {
            native.put_pixel(x + 3, y + 2, *pixel);
        }
        (
            DynamicImage::ImageRgba8(native),
            DynamicImage::ImageRgba8(page),
        )
    }

    #[test]
    fn transparent_window_corners_do_not_disqualify_an_opaque_unique_page_match() {
        let (mut native, page) = samples();
        native
            .as_mut_rgba8()
            .unwrap()
            .put_pixel(0, 0, Rgba([4, 8, 9, 38]));
        assert_eq!(
            match_samples(&native, &page),
            Ok(PixelPlacement {
                x: 3,
                y: 2,
                width: 2,
                height: 2
            })
        );
    }

    #[test]
    fn transparent_matched_pixels_or_page_pixels_cannot_prove_displayed_content() {
        let (mut native, mut page) = samples();
        native.as_mut_rgba8().unwrap().get_pixel_mut(3, 2)[3] = 38;
        assert_eq!(
            match_samples(&native, &page),
            Err("dom_pixel_native_alpha_unproven")
        );
        page.as_mut_rgba8().unwrap().get_pixel_mut(0, 0)[3] = 38;
        assert_eq!(
            match_samples(&native, &page),
            Err("dom_pixel_page_alpha_unproven")
        );
    }
}
