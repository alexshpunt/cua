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
    fn subregion_translation_cannot_extrapolate_into_unmatched_edges() {
        let (native, page, region) = rgba_fixture();
        let proof = match_region(&native, &page, region).unwrap();
        assert_eq!(
            proof.map_region(PixelPlacement {
                x: 2,
                y: 2,
                width: 1,
                height: 1
            }),
            Ok(PixelPlacement {
                x: 5,
                y: 4,
                width: 1,
                height: 1
            })
        );
        assert_eq!(proof.map_region(region), Ok(proof.native_pixel_region));
        for request in [
            PixelPlacement {
                x: 0,
                y: 1,
                width: 2,
                height: 2,
            },
            PixelPlacement {
                x: 1,
                y: 0,
                width: 2,
                height: 2,
            },
            PixelPlacement {
                x: 3,
                y: 3,
                width: 2,
                height: 1,
            },
            PixelPlacement {
                x: 2,
                y: 3,
                width: 1,
                height: 2,
            },
            PixelPlacement {
                x: 2,
                y: 2,
                width: 0,
                height: 1,
            },
            PixelPlacement {
                x: u32::MAX,
                y: 2,
                width: 2,
                height: 1,
            },
        ] {
            assert_eq!(
                proof.map_region(request),
                Err("dom_pixel_region_outside_proof")
            );
        }
    }
    fn rgba_fixture() -> (image::RgbaImage, image::RgbaImage, PixelPlacement) {
        let page = image::RgbaImage::from_fn(5, 5, |x, y| image::Rgba([x as u8, y as u8, 73, 255]));
        let mut native = image::RgbaImage::from_pixel(9, 8, image::Rgba([99, 99, 99, 0]));
        for y in 1..4 {
            for x in 1..4 {
                native.put_pixel(x + 3, y + 2, *page.get_pixel(x, y));
            }
        }
        (
            native,
            page,
            PixelPlacement {
                x: 1,
                y: 1,
                width: 3,
                height: 3,
            },
        )
    }

    #[test]
    fn declared_region_preserves_both_origins_without_admitting_unmatched_edges() {
        let (native, page, region) = rgba_fixture();
        assert_eq!(
            match_region(&native, &page, region),
            Ok(PixelCorrespondence {
                page_pixel_region: region,
                native_pixel_region: PixelPlacement {
                    x: 4,
                    y: 3,
                    width: 3,
                    height: 3
                },
            })
        );
        let rgb_native = image::DynamicImage::ImageRgba8(native).to_rgb8();
        let rgb_page = image::DynamicImage::ImageRgba8(page).to_rgb8();
        assert_eq!(
            match_viewport(&rgb_native, &rgb_page),
            Err("dom_pixel_match_not_found")
        );
    }

    #[test]
    fn region_matching_requires_exact_opaque_pixels_but_not_unrelated_native_corners() {
        let (mut native, mut page, region) = rgba_fixture();
        native.get_pixel_mut(4, 3).0[3] = 254;
        assert_eq!(
            match_region(&native, &page, region),
            Err("dom_pixel_region_not_opaque")
        );
        native.get_pixel_mut(4, 3).0[3] = 255;
        page.get_pixel_mut(1, 1).0[3] = 254;
        assert_eq!(
            match_region(&native, &page, region),
            Err("dom_pixel_region_not_opaque")
        );
        page.get_pixel_mut(1, 1).0[3] = 255;
        native.get_pixel_mut(4, 3).0[0] += 1;
        assert_eq!(
            match_region(&native, &page, region),
            Err("dom_pixel_match_not_found")
        );
    }

    #[test]
    fn invalid_declared_regions_refuse_before_search_or_allocation() {
        let (native, page, _) = rgba_fixture();
        for region in [
            PixelPlacement {
                x: 0,
                y: 0,
                width: 0,
                height: 1,
            },
            PixelPlacement {
                x: 5,
                y: 0,
                width: 1,
                height: 1,
            },
            PixelPlacement {
                x: 0,
                y: 4,
                width: 1,
                height: 2,
            },
            PixelPlacement {
                x: u32::MAX,
                y: 0,
                width: 2,
                height: 1,
            },
        ] {
            assert_eq!(
                match_region(&native, &page, region),
                Err("dom_pixel_region_invalid")
            );
        }
        assert_eq!(
            match_region(
                &image::RgbaImage::new(16_385, 1),
                &page,
                PixelPlacement {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1
                }
            ),
            Err("dom_pixel_image_limit")
        );
    }

    #[test]
    fn ambiguous_region_never_chooses_a_native_placement() {
        let (mut native, page, region) = rgba_fixture();
        for y in 0..3 {
            for x in 0..3 {
                native.put_pixel(x, y, *page.get_pixel(x + 1, y + 1));
            }
        }
        assert_eq!(
            match_region(&native, &page, region),
            Err("dom_pixel_match_ambiguous")
        );
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
