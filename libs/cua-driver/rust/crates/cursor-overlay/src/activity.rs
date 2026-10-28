//! Human-only activity metadata and bounded preview rendering. No capture or input authority.

use crate::session_badge::{font, rounded_rect as rounded};
use serde::{Deserialize, Serialize};
use tiny_skia::{Paint, Pixmap, PixmapPaint, Rect, Stroke, Transform};

pub const MAX_IMAGE_BASE64: usize = 12 * 1024 * 1024;
pub const TTL_MS: u64 = 1200;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ActivityRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}
impl ActivityRect {
    pub fn valid(self) -> bool {
        self.x.unsigned_abs() <= 2_000_000
            && self.y.unsigned_abs() <= 2_000_000
            && self.width > 0
            && self.height > 0
            && self.width <= 32768
            && self.height <= 32768
            && self.x.checked_add(self.width as i32).is_some()
            && self.y.checked_add(self.height as i32).is_some()
    }
    pub fn contains(self, other: Self) -> bool {
        self.valid()
            && other.valid()
            && other.x >= self.x
            && other.y >= self.y
            && i64::from(other.x) + i64::from(other.width)
                <= i64::from(self.x) + i64::from(self.width)
            && i64::from(other.y) + i64::from(other.height)
                <= i64::from(self.y) + i64::from(self.height)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ActivitySource {
    Window {
        window_id: u64,
        pid: u32,
        identity: String,
        bounds: ActivityRect,
    },
    Display {
        display: String,
        bounds: ActivityRect,
    },
}
impl ActivitySource {
    pub fn bounds(&self) -> ActivityRect {
        match self {
            Self::Window { bounds, .. } | Self::Display { bounds, .. } => *bounds,
        }
    }
    pub fn valid(&self) -> bool {
        if !self.bounds().valid() {
            return false;
        }
        match self {
            Self::Window {
                window_id,
                pid,
                identity,
                ..
            } => {
                *window_id > 0
                    && *window_id <= (1u64 << 53) - 1
                    && *pid > 0
                    && identity_property(identity, *pid).is_some()
            }
            Self::Display { display, .. } => {
                !display.is_empty()
                    && display.len() <= 256
                    && !display.chars().any(char::is_control)
            }
        }
    }
}

/// Return the property name and expected value without accepting alternate PID/lifetime spellings.
pub fn identity_property(identity: &str, pid: u32) -> Option<(&str, usize)> {
    let mut parts = identity.split(':');
    let property = parts.next()?;
    let owner = parts.next()?;
    let value = parts.next()?;
    if parts.next().is_some() || owner != pid.to_string() || value.starts_with('0') {
        return None;
    }
    let hex = property.strip_prefix("PiComputerUse.")?;
    if hex.len() != 32 || !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let value: usize = value.parse().ok()?;
    (value > 0).then_some((property, value))
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActivityKind {
    Overview,
    Detail,
    Inspect,
    Verify,
    Action,
    Clear,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityPoint {
    pub x: i32,
    pub y: i32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivityImage {
    pub data: String,
    pub mime_type: String,
}
// Image bytes and source metadata must never enter diagnostic Debug output.
#[derive(Clone, Serialize, Deserialize)]
pub struct ActivityEvent {
    pub generation: u64,
    pub kind: ActivityKind,
    pub source: Option<ActivitySource>,
    pub region: Option<ActivityRect>,
    pub point: Option<ActivityPoint>,
    pub image: Option<ActivityImage>,
    pub action: Option<String>,
    pub outcome: Option<String>,
}
impl ActivityEvent {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.generation == 0 || self.generation > (1u64 << 53) - 1 {
            return Err("invalid_generation");
        }
        if self.kind == ActivityKind::Clear {
            return if self.source.is_none()
                && self.image.is_none()
                && self.region.is_none()
                && self.point.is_none()
            {
                Ok(())
            } else {
                Err("invalid_clear")
            };
        }
        let source = self.source.as_ref().ok_or("source_required")?;
        if !source.valid() {
            return Err("invalid_source");
        }
        let bounds = source.bounds();
        if self.region.is_some_and(|r| !bounds.contains(r)) {
            return Err("region_outside_source");
        }
        if self.point.is_some_and(|p| {
            !bounds.contains(ActivityRect {
                x: p.x,
                y: p.y,
                width: 1,
                height: 1,
            })
        }) {
            return Err("point_outside_source");
        }
        if let Some(image) = &self.image {
            if image.data.is_empty()
                || image.data.len() > MAX_IMAGE_BASE64
                || !matches!(image.mime_type.as_str(), "image/png" | "image/jpeg")
            {
                return Err("invalid_preview");
            }
            if self.kind == ActivityKind::Action
                || matches!(self.outcome.as_deref(), Some("refused" | "unknown"))
            {
                return Err("preview_without_observation");
            }
        }
        if self.action.as_ref().is_some_and(|s| {
            s.len() > 64 || !s.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
        }) {
            return Err("invalid_action");
        }
        if self.outcome.as_ref().is_some_and(|s| {
            !matches!(s.as_str(), "delivered" | "refused" | "unknown" | "verified")
        }) {
            return Err("invalid_outcome");
        }
        Ok(())
    }
    pub fn caption(&self) -> &'static str {
        if self.outcome.as_deref() == Some("refused") {
            return "REFUSED";
        }
        if self.outcome.as_deref() == Some("unknown") {
            return "UNKNOWN";
        }
        match self.kind {
            ActivityKind::Overview => "READ",
            ActivityKind::Detail => "CROP",
            ActivityKind::Inspect => "INSPECT",
            ActivityKind::Verify => {
                if self.outcome.as_deref() == Some("verified") {
                    "VERIFIED"
                } else {
                    "CHECK"
                }
            }
            ActivityKind::Action => match self.action.as_deref() {
                Some("type_text" | "type" | "fill") => "TYPING",
                Some("press_key" | "hotkey" | "keypress") => "KEYS",
                Some("scroll" | "scroll_at") => "SCROLL",
                Some("drag" | "input_timeline") => "DRAG",
                _ => "ACTION",
            },
            ActivityKind::Clear => "",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct VisualSettings {
    pub preset: String,
    pub observations: bool,
    pub previews: bool,
    pub pointer: bool,
    pub scroll_drag: bool,
    pub keyboard: bool,
    pub opacity: f32,
    pub quality: String,
    pub reduced_motion: bool,
}
impl Default for VisualSettings {
    fn default() -> Self {
        Self {
            preset: "full".into(),
            observations: true,
            previews: true,
            pointer: true,
            scroll_drag: true,
            keyboard: true,
            opacity: 0.9,
            quality: "balanced".into(),
            reduced_motion: false,
        }
    }
}
impl VisualSettings {
    pub fn validate(&self) -> bool {
        matches!(self.preset.as_str(), "off" | "subtle" | "full")
            && self.opacity.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
            && matches!(self.quality.as_str(), "performance" | "balanced" | "high")
    }
    pub fn enabled(&self) -> bool {
        self.preset != "off" && self.opacity > 0.0
    }
    pub fn previews(&self) -> bool {
        self.enabled() && self.previews && self.preset == "full"
    }
    pub fn frame_ms(&self) -> u64 {
        match self.quality.as_str() {
            "performance" => 50,
            "high" => 16,
            _ => 33,
        }
    }
}

/// Decode only on the presentation worker; bound encoded bytes, decoder allocation and output size.
pub fn decode_preview(image: &ActivityImage) -> Option<Pixmap> {
    use base64::Engine;
    if image.data.len() > MAX_IMAGE_BASE64 {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&image.data)
        .ok()?;
    let format = match image.mime_type.as_str() {
        "image/png" => image::ImageFormat::Png,
        "image/jpeg" => image::ImageFormat::Jpeg,
        _ => return None,
    };
    let mut reader = image::ImageReader::with_format(std::io::Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?.thumbnail(240, 160).to_rgba8();
    let (w, h) = image.dimensions();
    let mut data = image.into_raw();
    for p in data.chunks_exact_mut(4) {
        for i in 0..3 {
            p[i] = ((u16::from(p[i]) * u16::from(p[3]) + 127) / 255) as u8;
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(w, h)?)
}

fn paint(r: u8, g: u8, b: u8, a: f32) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8);
    p.anti_alias = true;
    p
}

/// All positions are local to the protected source-sized native surface.
pub fn paint_activity(
    pixmap: &mut Pixmap,
    event: &ActivityEvent,
    preview: Option<&Pixmap>,
    settings: &VisualSettings,
    age_ms: u64,
    color: [u8; 4],
) {
    let Some(source) = &event.source else {
        return;
    };
    let bounds = source.bounds();
    let frame = event.region.unwrap_or(bounds);
    let age = age_ms as f32;
    let alpha = settings.opacity
        * if settings.reduced_motion {
            1.0
        } else {
            (age / 120.0).min(1.0) * ((TTL_MS as f32 - age) / 320.0).clamp(0.0, 1.0)
        };
    let (r, g, b) = match event.outcome.as_deref() {
        Some("refused" | "unknown") => (244, 178, 66),
        Some("verified") => (96, 218, 174),
        _ => (color[0], color[1], color[2]),
    };
    let rect = Rect::from_xywh(
        (frame.x - bounds.x) as f32 + 2.0,
        (frame.y - bounds.y) as f32 + 2.0,
        frame.width as f32 - 4.0,
        frame.height as f32 - 4.0,
    );
    if let Some(path) = rect.and_then(|r| rounded(r, 10.0)) {
        if settings.observations && event.kind != ActivityKind::Action {
            pixmap.fill_path(
                &path,
                &paint(r, g, b, alpha * 0.025),
                tiny_skia::FillRule::Winding,
                Transform::identity(),
                None,
            );
            if settings.quality != "performance" {
                for (width, opacity) in [(9.0, 0.035), (5.0, 0.07)] {
                    pixmap.stroke_path(
                        &path,
                        &paint(r, g, b, alpha * opacity),
                        &Stroke {
                            width,
                            ..Default::default()
                        },
                        Transform::identity(),
                        None,
                    );
                }
            }
            pixmap.stroke_path(
                &path,
                &paint(r, g, b, alpha * 0.8),
                &Stroke {
                    width: 1.6,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }
    let image = if settings.previews() { preview } else { None };
    let card_w = image
        .map_or(112.0, |p| p.width() as f32 + 16.0)
        .min(pixmap.width() as f32 - 12.0);
    let card_h = image.map_or(28.0, |p| p.height() as f32 + 38.0);
    if card_w < 36.0 || card_h > pixmap.height() as f32 - 12.0 {
        return;
    }
    // Prefer outside a detail region, otherwise tuck into a source corner.
    let mut x = (frame.x - bounds.x) as f32 + frame.width as f32 + 10.0;
    if x + card_w > pixmap.width() as f32 - 6.0 {
        x = ((frame.x - bounds.x) as f32 - card_w - 10.0).max(6.0);
    }
    let y = ((frame.y - bounds.y) as f32 + 8.0)
        .clamp(6.0, (pixmap.height() as f32 - card_h - 6.0).max(6.0));
    if let Some(path) = Rect::from_xywh(x, y, card_w, card_h).and_then(|r| rounded(r, 10.0)) {
        pixmap.fill_path(
            &path,
            &paint(16, 23, 34, alpha * 0.83),
            tiny_skia::FillRule::Winding,
            Transform::identity(),
            None,
        );
        pixmap.stroke_path(
            &path,
            &paint(r, g, b, alpha * 0.45),
            &Stroke {
                width: 1.0,
                ..Default::default()
            },
            Transform::identity(),
            None,
        );
    }
    if let Some(image) = image {
        if image.width() as f32 + 16.0 <= card_w {
            pixmap.draw_pixmap(
                (x + 8.0) as i32,
                (y + 30.0) as i32,
                image.as_ref(),
                &PixmapPaint {
                    opacity: alpha,
                    ..Default::default()
                },
                Transform::identity(),
                None,
            );
        }
    }
    caption(pixmap, event.caption(), x + 10.0, y + 8.0, alpha);
}

fn caption(pixmap: &mut Pixmap, text: &str, x: f32, y: f32, alpha: f32) {
    let Some(font) = font() else {
        return;
    };
    let mut dx = x as i32;
    for c in text.chars() {
        let (metrics, bitmap) = font.rasterize(c, 11.0);
        for yy in 0..metrics.height {
            for xx in 0..metrics.width {
                let px = dx + metrics.xmin + xx as i32;
                let py = y as i32 + 11 - metrics.height as i32 - metrics.ymin + yy as i32;
                if px >= 0
                    && py >= 0
                    && px < (pixmap.width() as i32)
                    && py < (pixmap.height() as i32)
                {
                    let a = bitmap[yy * metrics.width + xx] as f32 / 255.0 * alpha;
                    let Some(rect) = Rect::from_xywh(px as f32, py as f32, 1.0, 1.0) else {
                        continue;
                    };
                    pixmap.fill_rect(rect, &paint(231, 242, 250, a), Transform::identity(), None);
                }
            }
        }
        dx += metrics.advance_width.round() as i32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn event() -> ActivityEvent {
        serde_json::from_value(json!({"generation":1,"kind":"detail","source":{"window_id":10,"pid":2,"identity":"PiComputerUse.0123456789abcdef0123456789abcdef:2:123","bounds":{"x":-100,"y":0,"width":300,"height":200}},"region":{"x":-50,"y":10,"width":100,"height":80}})).unwrap()
    }
    #[test]
    fn source_identity_and_region_are_bounded() {
        let mut e = event();
        assert_eq!(e.validate(), Ok(()));
        e.region.as_mut().unwrap().x = -101;
        assert_eq!(e.validate(), Err("region_outside_source"));
        assert!(
            identity_property("PiComputerUse.0123456789abcdef0123456789abcdef:3:123", 2).is_none()
        );
        assert!(
            identity_property("PiComputerUse.0123456789abcdef0123456789abcdef:2:0", 2).is_none()
        );
    }
    #[test]
    fn refused_actions_cannot_carry_a_preview() {
        let mut e = event();
        e.image = Some(ActivityImage {
            data: "x".into(),
            mime_type: "image/png".into(),
        });
        e.outcome = Some("refused".into());
        assert_eq!(e.validate(), Err("preview_without_observation"));
        e.image = None;
    }
    #[test]
    fn malformed_and_oversized_preview_decoding_is_bounded() {
        assert!(decode_preview(&ActivityImage {
            data: "!bad".into(),
            mime_type: "image/png".into()
        })
        .is_none());
        assert!(decode_preview(&ActivityImage {
            data: "A".repeat(MAX_IMAGE_BASE64 + 1),
            mime_type: "image/png".into()
        })
        .is_none());
    }
    #[test]
    fn preview_preserves_aspect_and_has_a_small_output_budget() {
        use base64::Engine;
        let source = image::RgbaImage::from_pixel(480, 120, image::Rgba([200, 100, 50, 255]));
        let mut png = std::io::Cursor::new(Vec::new());
        source.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let p = decode_preview(&ActivityImage {
            data: base64::engine::general_purpose::STANDARD.encode(png.into_inner()),
            mime_type: "image/png".into(),
        })
        .unwrap();
        assert_eq!((p.width(), p.height()), (240, 60));
        assert_eq!(p.pixels()[0].red(), 200);
    }
    #[test]
    fn clear_cannot_smuggle_an_image_or_source() {
        let mut e = event();
        e.kind = ActivityKind::Clear;
        assert_eq!(e.validate(), Err("invalid_clear"));
    }
    #[test]
    fn reduced_motion_and_quality_are_explicit_settings() {
        let mut s = VisualSettings::default();
        assert!(s.previews());
        s.preset = "off".into();
        assert!(!s.enabled());
        s.preset = "subtle".into();
        assert!(!s.previews());
        s.opacity = f32::NAN;
        assert!(!s.validate());
    }
}
