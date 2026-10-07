//! Exact monitor selection. A topology snapshot is read geometry, never input authority.

use cua_driver_contract::DisplayInfo;
use sha2::{Digest, Sha256};

/// A native discovery snapshot. The token covers identity, geometry and scale.
#[derive(Debug)]
pub struct DisplayTopology {
    pub id: String,
    pub displays: Vec<DisplayInfo>,
}

/// Exact refusal vocabulary for selected-display reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayReadError {
    Unavailable,
    TopologyChanged,
    InvalidTopology,
}
impl DisplayReadError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unavailable => "display_unavailable",
            Self::TopologyChanged => "display_topology_changed",
            Self::InvalidTopology => "display_topology_invalid",
        }
    }
}
impl std::fmt::Display for DisplayReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for DisplayReadError {}

impl DisplayTopology {
    /// Canonicalize native enumeration without inventing a primary or missing metadata.
    pub fn new(mut displays: Vec<DisplayInfo>) -> Result<Self, DisplayReadError> {
        if displays.is_empty() {
            return Err(DisplayReadError::Unavailable);
        }
        displays.sort_by(|a, b| a.display_id.cmp(&b.display_id));
        if displays
            .windows(2)
            .any(|pair| pair[0].display_id == pair[1].display_id)
            || displays.iter().any(|d| {
                d.display_id.is_empty()
                    || d.monitor_handle.is_empty()
                    || d.bounds.width == 0
                    || d.bounds.height == 0
                    || d.bounds.width > i32::MAX as u32
                    || d.bounds.height > i32::MAX as u32
                    || d.bounds.x.checked_add(d.bounds.width as i32).is_none()
                    || d.bounds.y.checked_add(d.bounds.height as i32).is_none()
                    || d.scale_factor.is_some_and(|s| !s.is_finite() || s <= 0.0)
            })
        {
            return Err(DisplayReadError::InvalidTopology);
        }
        for display in &mut displays {
            display.monitor_device_paths.sort();
        }
        let bytes = serde_json::to_vec(&displays).map_err(|_| DisplayReadError::InvalidTopology)?;
        let id = format!("{:x}", Sha256::digest(bytes));
        Ok(Self { id, displays })
    }

    /// Select by exact native identity only after verifying the whole topology.
    pub fn select(
        &self,
        display_id: &str,
        topology_id: &str,
    ) -> Result<&DisplayInfo, DisplayReadError> {
        if self.id != topology_id {
            return Err(DisplayReadError::TopologyChanged);
        }
        self.displays
            .iter()
            .find(|d| d.display_id == display_id)
            .ok_or(DisplayReadError::Unavailable)
    }
}
#[cfg(target_os = "windows")]
mod native;
#[cfg(target_os = "windows")]
pub use native::{capture, enumerate};
#[cfg(test)]
mod tests {
    use super::*;
    use cua_driver_contract::{DisplayBounds, DisplayInfo};

    fn monitor(id: &str, x: i32, scale: Option<f64>) -> DisplayInfo {
        DisplayInfo {
            display_id: id.into(),
            monitor_handle: format!("handle-{id}"),
            monitor_device_paths: vec![format!("path-{id}")],
            bounds: DisplayBounds {
                x,
                y: -200,
                width: 1920,
                height: 1080,
            },
            work_area: DisplayBounds {
                x,
                y: -200,
                width: 1920,
                height: 1040,
            },
            primary: x == 0,
            scale_factor: scale,
            dpi: None,
        }
    }

    #[test]
    fn exact_selection_keeps_negative_origin_and_unknown_dpi() {
        let topology = DisplayTopology::new(vec![
            monitor("primary", 0, Some(1.0)),
            monitor("left", -1920, Some(1.5)),
        ])
        .unwrap();
        let selected = topology.select("left", &topology.id).unwrap();
        assert_eq!(selected.bounds.x, -1920);
        assert_eq!(selected.bounds.y, -200);
        assert_eq!(selected.scale_factor, Some(1.5));
        assert_eq!(selected.dpi, None);
        assert_eq!(
            topology.select("missing", &topology.id),
            Err(DisplayReadError::Unavailable)
        );
        assert_eq!(
            topology.select("primary", "old"),
            Err(DisplayReadError::TopologyChanged)
        );
    }

    #[test]
    fn topology_tracks_identity_geometry_scale_and_work_area_not_enumeration_order() {
        let a = monitor("primary", 0, Some(1.0));
        let b = monitor("left", -1920, None);
        let original = DisplayTopology::new(vec![a.clone(), b.clone()]).unwrap();
        let reordered = DisplayTopology::new(vec![b.clone(), a.clone()]).unwrap();
        assert_eq!(original.id, reordered.id);
        for field in ["bounds", "work_area", "scale", "handle", "paths"] {
            let mut changed = b.clone();
            match field {
                "bounds" => changed.bounds.x -= 1,
                "work_area" => changed.work_area.height -= 1,
                "scale" => changed.scale_factor = Some(1.25),
                "handle" => changed.monitor_handle = "replacement".into(),
                "paths" => changed.monitor_device_paths = vec!["replacement".into()],
                _ => unreachable!(),
            }
            let current = DisplayTopology::new(vec![a.clone(), changed]).unwrap();
            assert_eq!(
                current.select("left", &original.id),
                Err(DisplayReadError::TopologyChanged)
            );
        }
        let removed = DisplayTopology::new(vec![a]).unwrap();
        assert_eq!(
            removed.select("left", &original.id),
            Err(DisplayReadError::TopologyChanged)
        );
    }

    #[test]
    fn ambiguous_or_invalid_native_geometry_is_not_a_capture_target() {
        let a = monitor("same", 0, None);
        assert_eq!(
            DisplayTopology::new(vec![a.clone(), a.clone()]).unwrap_err(),
            DisplayReadError::InvalidTopology
        );
        let mut invalid = a;
        invalid.bounds.width = 0;
        assert_eq!(
            DisplayTopology::new(vec![invalid]).unwrap_err(),
            DisplayReadError::InvalidTopology
        );
        assert_eq!(
            DisplayTopology::new(vec![]).unwrap_err(),
            DisplayReadError::Unavailable
        );
    }
}
