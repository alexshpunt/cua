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

fn rgb(pixel: &[u8]) -> u64 {
    ((u64::from(pixel[0]) << 16) | (u64::from(pixel[1]) << 8) | u64::from(pixel[2])) + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn page() -> RgbImage {
        RgbImage::from_fn(3, 3, |x, y| {
            Rgb([10 + x as u8, 30 + y as u8, (x + y) as u8])
        })
    }

    fn paste(native: &mut RgbImage, page: &RgbImage, x: u32, y: u32) {
        for (px, py, pixel) in page.enumerate_pixels() {
            native.put_pixel(x + px, y + py, *pixel);
        }
    }

    #[test]
    fn unique_full_page_match_preserves_native_pixel_offset_and_size() {
        let page = page();
        let mut native = RgbImage::from_pixel(9, 8, Rgb([99, 99, 99]));
        paste(&mut native, &page, 4, 3);
        assert_eq!(
            match_viewport(&native, &page),
            Ok(PixelPlacement {
                x: 4,
                y: 3,
                width: 3,
                height: 3
            })
        );
        assert_eq!(
            match_viewport(&page, &page),
            Ok(PixelPlacement {
                x: 0,
                y: 0,
                width: 3,
                height: 3
            })
        );
    }

    #[test]
    fn duplicate_pages_and_uniform_multiple_placements_refuse_ambiguity() {
        let page = page();
        let mut native = RgbImage::from_pixel(9, 8, Rgb([99, 99, 99]));
        paste(&mut native, &page, 0, 0);
        paste(&mut native, &page, 5, 4);
        assert_eq!(
            match_viewport(&native, &page),
            Err("dom_pixel_match_ambiguous")
        );
        let uniform = RgbImage::from_pixel(5, 5, Rgb([0, 0, 0]));
        assert_eq!(
            match_viewport(&uniform, &RgbImage::new(2, 2)),
            Err("dom_pixel_match_ambiguous")
        );
    }

    #[test]
    fn a_single_changed_pixel_cannot_pass_as_correspondence() {
        let page = page();
        let mut native = page.clone();
        native.put_pixel(0, 0, Rgb([200, 0, 0]));
        assert_eq!(
            match_viewport(&native, &page),
            Err("dom_pixel_match_not_found")
        );
        assert_eq!(
            match_viewport(&RgbImage::new(5, 5), &page),
            Err("dom_pixel_match_not_found")
        );
    }

    #[test]
    fn dimensions_and_image_limits_refuse_before_matching() {
        let page = page();
        assert_eq!(
            match_viewport(&RgbImage::new(0, 3), &page),
            Err("dom_pixel_invalid_dimensions")
        );
        assert_eq!(
            match_viewport(&page, &RgbImage::new(3, 0)),
            Err("dom_pixel_invalid_dimensions")
        );
        assert_eq!(
            match_viewport(&page, &RgbImage::new(4, 3)),
            Err("dom_pixel_page_outside_native")
        );
        assert_eq!(
            match_viewport(&RgbImage::new(16_385, 1), &RgbImage::new(1, 1)),
            Err("dom_pixel_image_limit")
        );
    }

    #[test]
    fn repeated_anchor_rows_still_require_every_page_pixel() {
        let mut page = RgbImage::new(3, 3);
        page.put_pixel(0, 0, Rgb([7, 8, 9]));
        page.put_pixel(2, 2, Rgb([3, 4, 5]));
        let mut native = RgbImage::new(9, 7);
        paste(&mut native, &page, 0, 0);
        native.put_pixel(2, 2, Rgb([3, 4, 6]));
        paste(&mut native, &page, 5, 3);
        assert_eq!(
            match_viewport(&native, &page),
            Ok(PixelPlacement {
                x: 5,
                y: 3,
                width: 3,
                height: 3
            })
        );
    }

    #[test]
    fn informative_rows_avoid_budget_exhaustion_on_blank_page_backgrounds() {
        let mut page = RgbImage::from_pixel(200, 100, Rgb([255, 255, 255]));
        for x in 0..200 {
            page.put_pixel(x, 1, Rgb([x as u8, 7, 9]));
        }
        let mut native = RgbImage::from_pixel(210, 130, Rgb([255, 255, 255]));
        paste(&mut native, &page, 5, 20);
        assert_eq!(
            match_viewport(&native, &page),
            Ok(PixelPlacement {
                x: 5,
                y: 20,
                width: 200,
                height: 100
            })
        );
    }
    #[test]
    fn exhausted_search_never_becomes_absence_or_unique_evidence() {
        let mut page = RgbImage::new(1, 257);
        page.put_pixel(0, 256, Rgb([1, 0, 0]));
        let mut native = RgbImage::new(257, 513);
        assert_eq!(
            match_viewport(&native, &page),
            Err("dom_pixel_match_budget")
        );
        paste(&mut native, &page, 0, 0);
        assert_eq!(
            match_viewport(&native, &page),
            Err("dom_pixel_match_budget"),
            "One found occurrence is not unique evidence until the bounded search completes."
        );
    }
}
