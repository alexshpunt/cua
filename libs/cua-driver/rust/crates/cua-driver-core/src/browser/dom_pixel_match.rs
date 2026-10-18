//! Bounded exact RGB correspondence, not browser identity or input authority.

use image::RgbImage;

/// One uniquely proven, unscaled page bitmap inside a native window bitmap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelPlacement {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Find exactly one full-page RGB occurrence without resampling or tolerances.
///
/// Callers must independently prove the same current window, selected tab and
/// document. A match only relates these two bitmap samples; it does not make
/// their capture atomic or grant element/input authority.
///
/// Images are limited to 16,777,216 pixels and a 16,384-pixel edge. Rolling
/// hashes only filter candidates; at most 128 candidates and 67,108,864 pixels
/// are directly compared. Exhaustion refuses even after finding one occurrence.
/// Refusals distinguish invalid dimensions, image limits, an oversized page,
/// no match, ambiguity and an exhausted match budget.
pub fn match_viewport(native: &RgbImage, page: &RgbImage) -> Result<PixelPlacement, &'static str> {
    const MAX_EDGE: u32 = 16_384;
    const MAX_PIXELS: u64 = 16_777_216;
    const MAX_CANDIDATES: usize = 128;
    const MAX_COMPARED_PIXELS: u64 = 67_108_864;
    const BASE: u64 = 1_099_511_628_211;

    for image in [native, page] {
        let (width, height) = image.dimensions();
        let pixels = u64::from(width) * u64::from(height);
        if width == 0 || height == 0 {
            return Err("dom_pixel_invalid_dimensions");
        }
        if width > MAX_EDGE || height > MAX_EDGE || pixels > MAX_PIXELS {
            return Err("dom_pixel_image_limit");
        }
        if image.as_raw().len() as u64 != pixels * 3 {
            return Err("dom_pixel_invalid_dimensions");
        }
    }
    let (nw, nh) = native.dimensions();
    let (pw, ph) = page.dimensions();
    if pw > nw || ph > nh {
        return Err("dom_pixel_page_outside_native");
    }
    let stride = nw as usize * 3;
    let page_stride = pw as usize * 3;
    let native_bytes = native.as_raw();
    let page_bytes = page.as_raw();
    // A busy row filters blank backgrounds without weakening full verification.
    let mut anchor_y = 0;
    let mut best_changes = 0;
    for (y, row) in page_bytes.chunks_exact(page_stride).enumerate() {
        let changes = row
            .chunks_exact(3)
            .zip(row[3..].chunks_exact(3))
            .filter(|(left, right)| left != right)
            .count();
        if changes > best_changes {
            anchor_y = y as u32;
            best_changes = changes;
        }
        if changes == pw as usize - 1 {
            break;
        }
    }
    let anchor_offset = anchor_y as usize * page_stride;
    let mut power = 1u64;
    for _ in 1..pw {
        power = power.wrapping_mul(BASE);
    }
    let hash = |bytes: &[u8]| {
        bytes.chunks_exact(3).fold(0u64, |hash, pixel| {
            hash.wrapping_mul(BASE).wrapping_add(rgb(pixel))
        })
    };
    let wanted = hash(&page_bytes[anchor_offset..anchor_offset + page_stride]);
    let mut candidates = 0;
    let mut compared = 0u64;
    let mut found = None;
    for y in 0..=nh - ph {
        let row = (y + anchor_y) as usize * stride;
        let mut rolling = hash(&native_bytes[row..row + page_stride]);
        for x in 0..=nw - pw {
            if rolling == wanted {
                candidates += 1;
                if candidates > MAX_CANDIDATES {
                    return Err("dom_pixel_match_budget");
                }
                // Hashes only discover candidates. Every RGB byte must agree.
                let mut exact = true;
                for py in 0..ph {
                    compared += u64::from(pw);
                    if compared > MAX_COMPARED_PIXELS {
                        return Err("dom_pixel_match_budget");
                    }
                    let native_offset = (y + py) as usize * stride + x as usize * 3;
                    let page_offset = py as usize * page_stride;
                    if native_bytes[native_offset..native_offset + page_stride]
                        != page_bytes[page_offset..page_offset + page_stride]
                    {
                        exact = false;
                        break;
                    }
                }
                if exact {
                    if found.is_some() {
                        return Err("dom_pixel_match_ambiguous");
                    }
                    found = Some(PixelPlacement {
                        x,
                        y,
                        width: pw,
                        height: ph,
                    });
                }
            }
            if x < nw - pw {
                let offset = row + x as usize * 3;
                rolling = rolling
                    .wrapping_sub(rgb(&native_bytes[offset..offset + 3]).wrapping_mul(power))
                    .wrapping_mul(BASE)
                    .wrapping_add(rgb(
                        &native_bytes[offset + page_stride..offset + page_stride + 3]
                    ));
            }
        }
    }
    found.ok_or("dom_pixel_match_not_found")
}

/// Exact correspondence for one explicitly requested page-bitmap region.
///
/// This does not describe the rest of the page, CSS geometry, source identity
/// or simultaneous paint. Consumers must not extrapolate outside either region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelCorrespondence {
    pub page_pixel_region: PixelPlacement,
    pub native_pixel_region: PixelPlacement,
}

impl PixelCorrespondence {
    /// Translate a page-bitmap rectangle only when it is wholly proven.
    /// Input uses page-bitmap pixels, not CSS. Rectangles crossing the proof
    /// boundary are refused without clipping or extrapolating unmatched pixels.
    pub fn map_region(&self, requested: PixelPlacement) -> Result<PixelPlacement, &'static str> {
        let page = self.page_pixel_region;
        let native = self.native_pixel_region;
        let refusal = "dom_pixel_region_outside_proof";
        for region in [page, native, requested] {
            if region.width == 0
                || region.height == 0
                || region.x.checked_add(region.width).is_none()
                || region.y.checked_add(region.height).is_none()
            {
                return Err(refusal);
            }
        }
        if page.width != native.width || page.height != native.height {
            return Err(refusal);
        }
        let x = requested.x.checked_sub(page.x).ok_or(refusal)?;
        let y = requested.y.checked_sub(page.y).ok_or(refusal)?;
        if x.checked_add(requested.width)
            .is_none_or(|right| right > page.width)
            || y.checked_add(requested.height)
                .is_none_or(|bottom| bottom > page.height)
        {
            return Err(refusal);
        }
        Ok(PixelPlacement {
            x: native.x.checked_add(x).ok_or(refusal)?,
            y: native.y.checked_add(y).ok_or(refusal)?,
            width: requested.width,
            height: requested.height,
        })
    }
}
/// Match a declared opaque RGBA region without retrying smaller regions.
///
/// The full-image matcher remains separate. This operation never chooses an
/// inset, tolerates changed channels, rescales pixels or grants input authority.
pub fn match_region(
    native: &image::RgbaImage,
    page: &image::RgbaImage,
    page_pixel_region: PixelPlacement,
) -> Result<PixelCorrespondence, &'static str> {
    for image in [native, page] {
        let (width, height) = image.dimensions();
        if width == 0 || height == 0 {
            return Err("dom_pixel_invalid_dimensions");
        }
        if width > 16_384 || height > 16_384 || u64::from(width) * u64::from(height) > 16_777_216 {
            return Err("dom_pixel_image_limit");
        }
    }
    let region = page_pixel_region;
    if region.width == 0
        || region.height == 0
        || region
            .x
            .checked_add(region.width)
            .is_none_or(|right| right > page.width())
        || region
            .y
            .checked_add(region.height)
            .is_none_or(|bottom| bottom > page.height())
    {
        return Err("dom_pixel_region_invalid");
    }
    for y in region.y..region.y + region.height {
        for x in region.x..region.x + region.width {
            if page.get_pixel(x, y).0[3] != 255 {
                return Err("dom_pixel_region_not_opaque");
            }
        }
    }
    let native_rgb = RgbImage::from_fn(native.width(), native.height(), |x, y| {
        let pixel = native.get_pixel(x, y).0;
        image::Rgb([pixel[0], pixel[1], pixel[2]])
    });
    let page_rgb = RgbImage::from_fn(region.width, region.height, |x, y| {
        let pixel = page.get_pixel(region.x + x, region.y + y).0;
        image::Rgb([pixel[0], pixel[1], pixel[2]])
    });
    // Reuse the complete bounded uniqueness search. A transparent duplicate
    // can only cause conservative refusal, never weaken exact correspondence.
    let native_pixel_region = match_viewport(&native_rgb, &page_rgb)?;
    for y in native_pixel_region.y..native_pixel_region.y + native_pixel_region.height {
        for x in native_pixel_region.x..native_pixel_region.x + native_pixel_region.width {
            if native.get_pixel(x, y).0[3] != 255 {
                return Err("dom_pixel_region_not_opaque");
            }
        }
    }
    Ok(PixelCorrespondence {
        page_pixel_region,
        native_pixel_region,
    })
}
fn rgb(pixel: &[u8]) -> u64 {
    ((u64::from(pixel[0]) << 16) | (u64::from(pixel[1]) << 8) | u64::from(pixel[2])) + 1
}
