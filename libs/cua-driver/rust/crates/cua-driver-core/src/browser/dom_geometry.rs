//! Read-only mapping from a proven main-document viewport to native pixels.
//! This does not mint pointer or keyboard authority.
use super::types::Rect;

/// Independently measured native browser content area, in physical desktop pixels.
#[derive(Clone, Debug)]
pub struct NativeBrowserViewport {
    pub window: Rect,
    pub viewport: Rect,
    pub source: &'static str,
}

/// A clipped visible element rectangle in physical desktop pixels.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MappedDomBounds {
    pub screen: Rect,
    pub clipped: bool,
}

/// Map document CSS pixels only when independently measured viewport dimensions
/// agree with the page's scale. Child frames require a separate proven transform.
pub(crate) fn map_document_bounds(
    element: &Rect,
    css_viewport: &Rect,
    native: &NativeBrowserViewport,
    device_pixel_ratio: f64,
) -> Result<MappedDomBounds, &'static str> {
    let valid = |rect: &Rect| [rect.x, rect.y, rect.width, rect.height,
        rect.x + rect.width, rect.y + rect.height].iter().all(|n| n.is_finite())
        && rect.width > 0.0 && rect.height > 0.0;
    if !valid(element) || !valid(css_viewport) || !device_pixel_ratio.is_finite()
        || !(0.1..=16.0).contains(&device_pixel_ratio) {
        return Err("dom_css_geometry_invalid");
    }
    if !valid(&native.window) || !valid(&native.viewport)
        || native.viewport.x < native.window.x || native.viewport.y < native.window.y
        || native.viewport.x + native.viewport.width > native.window.x + native.window.width
        || native.viewport.y + native.viewport.height > native.window.y + native.window.height {
        return Err("dom_native_viewport_invalid");
    }
    // A small integer-rounding tolerance is not a guessed browser inset.
    if (css_viewport.width * device_pixel_ratio - native.viewport.width).abs() > 1.0
        || (css_viewport.height * device_pixel_ratio - native.viewport.height).abs() > 1.0 {
        return Err("dom_viewport_scale_mismatch");
    }
    let left = element.x.max(css_viewport.x);
    let top = element.y.max(css_viewport.y);
    let right = (element.x + element.width).min(css_viewport.x + css_viewport.width);
    let bottom = (element.y + element.height).min(css_viewport.y + css_viewport.height);
    if right <= left || bottom <= top { return Err("dom_element_outside_viewport"); }
    let scale_x = native.viewport.width / css_viewport.width;
    let scale_y = native.viewport.height / css_viewport.height;
    Ok(MappedDomBounds {
        screen: Rect {
            x: native.viewport.x + (left - css_viewport.x) * scale_x,
            y: native.viewport.y + (top - css_viewport.y) * scale_y,
            width: (right - left) * scale_x,
            height: (bottom - top) * scale_y,
        },
        clipped: left != element.x || top != element.y
            || right != element.x + element.width || bottom != element.y + element.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
        Rect { x, y, width, height }
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
        let mapped = map_document_bounds(&rect(40.0, 220.0, 120.0, 30.0),
            &rect(0.0, 200.0, 888.0, 554.0), &native(), 1.5).unwrap();
        assert_eq!(mapped.screen, rect(-1371.0, 180.0, 180.0, 45.0));
        assert!(!mapped.clipped);
    }
    #[test]
    fn offscreen_elements_are_refused_and_partial_elements_are_explicitly_clipped() {
        let viewport = rect(0.0, 200.0, 888.0, 554.0);
        assert_eq!(map_document_bounds(&rect(10.0, 100.0, 20.0, 30.0), &viewport, &native(), 1.5),
            Err("dom_element_outside_viewport"));
        let mapped = map_document_bounds(&rect(-10.0, 190.0, 30.0, 30.0), &viewport, &native(), 1.5).unwrap();
        assert_eq!(mapped.screen, rect(-1431.0, 150.0, 30.0, 30.0));
        assert!(mapped.clipped);
    }
    #[test]
    fn unproven_geometry_is_not_replaced_with_chrome_inset_guesses() {
        let viewport = rect(0.0, 0.0, 888.0, 554.0);
        let element = rect(10.0, 10.0, 20.0, 30.0);
        assert_eq!(map_document_bounds(&element, &viewport, &native(), 1.0), Err("dom_viewport_scale_mismatch"));
        let mut invalid = native();
        invalid.viewport.x = invalid.window.x - 1.0;
        assert_eq!(map_document_bounds(&element, &viewport, &invalid, 1.5), Err("dom_native_viewport_invalid"));
        assert_eq!(map_document_bounds(&element, &viewport, &native(), f64::NAN), Err("dom_css_geometry_invalid"));
        assert_eq!(map_document_bounds(&rect(0.0, 0.0, -1.0, 2.0), &viewport, &native(), 1.5), Err("dom_css_geometry_invalid"));
    }
}
