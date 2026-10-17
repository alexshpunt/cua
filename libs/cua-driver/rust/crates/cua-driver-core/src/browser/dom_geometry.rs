//! Readonly physical enclosing geometry. This never mints input or paint authority.
use super::types::Rect;

/// Independently measured top-level native root web area, in physical screen pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct NativeBrowserViewport {
    /// Exact owning native window bounds in physical screen pixels.
    pub window: Rect,
    /// Unclipped native content viewport, never inferred from browser chrome.
    pub viewport: Rect,
    /// Native measurement method; the candidate requires `uia_root_web_area`.
    pub source: &'static str,
    /// Opaque native identity, compared for continuity only; never joined to DOM IDs.
    pub identity: Vec<i32>,
}

/// Layout viewport facts from the same attested isolated-world read as the element.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CssViewport {
    pub width: f64,
    pub height: f64,
    pub dpr: f64,
    pub visual_scale: f64,
    pub offset_x: f64,
    pub offset_y: f64,
}

/// A clipped physical enclosing rectangle, not a claim about raster edge colors.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MappedDomBounds {
    pub screen: Rect,
    pub clipped: bool,
}

/// Map current layout-viewport CSS using sampled DPR only. Native extents must
/// agree exactly; neither a dimension ratio nor an integer tolerance is accepted.
/// Enclose fractional edges with floor/ceil before intersecting native viewport.
pub(crate) fn map_layout_bounds(
    element: &Rect,
    css: CssViewport,
    native: &NativeBrowserViewport,
) -> Result<MappedDomBounds, &'static str> {
    let valid = |rect: &Rect| {
        [
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            rect.x + rect.width,
            rect.y + rect.height,
        ]
        .iter()
        .all(|n| n.is_finite() && n.abs() <= 1_000_000_000.0)
            && rect.width > 0.0
            && rect.height > 0.0
    };
    if !valid(element)
        || !css.width.is_finite()
        || !css.height.is_finite()
        || css.width <= 0.0
        || css.height <= 0.0
        || css.width > 1_000_000_000.0
        || css.height > 1_000_000_000.0
        || !css.dpr.is_finite()
        || !(0.1..=16.0).contains(&css.dpr)
    {
        return Err("dom_css_geometry_invalid");
    }
    if css.visual_scale != 1.0 || css.offset_x != 0.0 || css.offset_y != 0.0 {
        return Err("dom_visual_viewport_unqualified");
    }
    if native.source != "uia_root_web_area"
        || native.identity.is_empty()
        || native.identity.len() > 64
    {
        return Err("dom_native_viewport_unqualified");
    }
    if !valid(&native.window)
        || !valid(&native.viewport)
        || [
            native.viewport.x,
            native.viewport.y,
            native.viewport.width,
            native.viewport.height,
        ]
        .iter()
        .any(|n| n.fract() != 0.0)
        || native.viewport.x < native.window.x
        || native.viewport.y < native.window.y
        || native.viewport.x + native.viewport.width > native.window.x + native.window.width
        || native.viewport.y + native.viewport.height > native.window.y + native.window.height
    {
        return Err("dom_native_viewport_invalid");
    }
    if css.width * css.dpr != native.viewport.width
        || css.height * css.dpr != native.viewport.height
    {
        return Err("dom_viewport_scale_mismatch");
    }
    // The element is already layout-viewport relative. Never subtract document scroll.
    let left = element.x.max(0.0);
    let top = element.y.max(0.0);
    let right = (element.x + element.width).min(css.width);
    let bottom = (element.y + element.height).min(css.height);
    if right <= left || bottom <= top {
        return Err("dom_element_outside_viewport");
    }
    let x = native.viewport.x + (left * css.dpr).floor();
    let y = native.viewport.y + (top * css.dpr).floor();
    let end_x = native.viewport.x + (right * css.dpr).ceil();
    let end_y = native.viewport.y + (bottom * css.dpr).ceil();
    Ok(MappedDomBounds {
        screen: Rect {
            x,
            y,
            width: end_x - x,
            height: end_y - y,
        },
        clipped: left != element.x
            || top != element.y
            || right != element.x + element.width
            || bottom != element.y + element.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }
    fn native() -> NativeBrowserViewport {
        NativeBrowserViewport {
            window: rect(-1440.0, 30.0, 1350.0, 960.0),
            viewport: rect(-1431.0, 150.0, 1332.0, 831.0),
            source: "uia_root_web_area",
            identity: vec![42, 7, 1],
        }
    }
    fn css() -> CssViewport {
        CssViewport {
            width: 888.0,
            height: 554.0,
            dpr: 1.5,
            visual_scale: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
    #[test]
    fn sampled_dpr_maps_current_viewport_bounds_at_a_negative_screen_origin() {
        let mapped = map_layout_bounds(&rect(40.0, 20.0, 120.0, 30.0), css(), &native()).unwrap();
        assert_eq!(mapped.screen, rect(-1371.0, 180.0, 180.0, 45.0));
        assert!(!mapped.clipped);
    }
    #[test]
    fn fractional_edges_are_enclosed_without_rounding_the_scale_or_fitting_an_offset() {
        let mapped = map_layout_bounds(&rect(0.5, 1.25, 2.0, 2.5), css(), &native()).unwrap();
        assert_eq!(mapped.screen, rect(-1431.0, 151.0, 4.0, 5.0));
    }
    #[test]
    fn even_one_physical_pixel_of_viewport_disagreement_refuses() {
        for delta in [-1.0, 1.0] {
            let mut sample = native();
            sample.viewport.height += delta;
            assert_eq!(
                map_layout_bounds(&rect(1.0, 1.0, 10.0, 10.0), css(), &sample),
                Err("dom_viewport_scale_mismatch")
            );
        }
    }
    #[test]
    fn visual_zoom_and_offsets_are_not_silently_treated_as_layout_coordinates() {
        let bounds = rect(1.0, 1.0, 10.0, 10.0);
        for changed in [
            CssViewport {
                visual_scale: 1.25,
                ..css()
            },
            CssViewport {
                offset_x: 0.25,
                ..css()
            },
            CssViewport {
                offset_y: 1.0,
                ..css()
            },
        ] {
            assert_eq!(
                map_layout_bounds(&bounds, changed, &native()),
                Err("dom_visual_viewport_unqualified")
            );
        }
    }
    #[test]
    fn clipping_is_explicit_and_offscreen_geometry_refuses() {
        assert_eq!(
            map_layout_bounds(&rect(1.0, -100.0, 2.0, 3.0), css(), &native()),
            Err("dom_element_outside_viewport")
        );
        let clipped = map_layout_bounds(&rect(-10.0, -20.0, 30.0, 40.0), css(), &native()).unwrap();
        assert!(clipped.clipped);
        assert_eq!(clipped.screen, rect(-1431.0, 150.0, 30.0, 30.0));
    }
    #[test]
    fn a_legacy_widget_unknown_identity_or_unbounded_geometry_is_not_a_native_root() {
        let bounds = rect(1.0, 1.0, 10.0, 10.0);
        let mut sample = native();
        sample.source = "win32_render_widget_client";
        assert_eq!(
            map_layout_bounds(&bounds, css(), &sample),
            Err("dom_native_viewport_unqualified")
        );
        sample = native();
        sample.identity.clear();
        assert_eq!(
            map_layout_bounds(&bounds, css(), &sample),
            Err("dom_native_viewport_unqualified")
        );
        assert_eq!(
            map_layout_bounds(&rect(f64::NAN, 1.0, 1.0, 1.0), css(), &native()),
            Err("dom_css_geometry_invalid")
        );
    }
}
