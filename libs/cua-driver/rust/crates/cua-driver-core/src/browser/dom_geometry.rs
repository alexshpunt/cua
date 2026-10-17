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
    #[test]
    fn an_unmeasured_candidate_cannot_establish_native_viewport_uniqueness() {
        let bounds = rect(9.0, 120.0, 1332.0, 831.0);
        assert_eq!(
            unique_native_viewport(&[Some(bounds), None]),
            Err("dom_native_viewport_unavailable")
        );
        assert_eq!(
            unique_native_viewport(&[]),
            Err("dom_native_viewport_ambiguous")
        );
        assert_eq!(
            unique_native_viewport(&[Some(bounds), Some(bounds)]),
            Err("dom_native_viewport_ambiguous")
        );
        assert_eq!(unique_native_viewport(&[Some(bounds)]), Ok(bounds));
    }
    fn native() -> NativeBrowserViewport {
        NativeBrowserViewport {
            window: rect(-1440.0, 30.0, 1350.0, 960.0),
            viewport: rect(-1431.0, 150.0, 1332.0, 831.0),
            source: "win32_render_widget_client",
        }
    }
    #[test]
    fn measured_viewport_maps_scrolled_css_at_non_unit_dpi_and_negative_desktop_origin() {
        let mapped = map_document_bounds(
            &rect(40.0, 220.0, 120.0, 30.0),
            &rect(0.0, 200.0, 888.0, 554.0),
            &native(),
            1.5,
        )
        .unwrap();
        assert_eq!(mapped.screen, rect(-1371.0, 180.0, 180.0, 45.0));
        assert!(!mapped.clipped);
    }
    #[test]
    fn offscreen_elements_are_refused_and_partial_elements_are_explicitly_clipped() {
        let viewport = rect(0.0, 200.0, 888.0, 554.0);
        assert_eq!(
            map_document_bounds(&rect(10.0, 100.0, 20.0, 30.0), &viewport, &native(), 1.5),
            Err("dom_element_outside_viewport")
        );
        let mapped =
            map_document_bounds(&rect(-10.0, 190.0, 30.0, 30.0), &viewport, &native(), 1.5)
                .unwrap();
        assert_eq!(mapped.screen, rect(-1431.0, 150.0, 30.0, 30.0));
        assert!(mapped.clipped);
    }
    #[test]
    fn unproven_geometry_is_not_replaced_with_chrome_inset_guesses() {
        let viewport = rect(0.0, 0.0, 888.0, 554.0);
        let element = rect(10.0, 10.0, 20.0, 30.0);
        assert_eq!(
            map_document_bounds(&element, &viewport, &native(), 1.0),
            Err("dom_viewport_scale_mismatch")
        );
        let mut invalid = native();
        invalid.viewport.x = invalid.window.x - 1.0;
        assert_eq!(
            map_document_bounds(&element, &viewport, &invalid, 1.5),
            Err("dom_native_viewport_invalid")
        );
        assert_eq!(
            map_document_bounds(&element, &viewport, &native(), f64::NAN),
            Err("dom_css_geometry_invalid")
        );
        assert_eq!(
            map_document_bounds(&rect(0.0, 0.0, -1.0, 2.0), &viewport, &native(), 1.5),
            Err("dom_css_geometry_invalid")
        );
    }
}
