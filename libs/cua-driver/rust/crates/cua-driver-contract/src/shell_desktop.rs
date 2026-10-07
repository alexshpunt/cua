//! Read-only Windows shell desktop membership, independent of window lifetime and input authority.

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// Native query failure. HRESULT is kept as its signed 32-bit value when Windows supplied one.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct DesktopQueryError {
    pub code: DesktopQueryErrorCode,
    pub hresult: Option<i32>,
}

/// Failure stages do not stand in for the current desktop or a successful query.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, uniffi::Enum)]
#[serde(rename_all = "snake_case")]
pub enum DesktopQueryErrorCode {
    ComInitializationFailed,
    ManagerUnavailable,
    WindowUnavailable,
    DesktopIdQueryFailed,
    CurrentDesktopQueryFailed,
    InvalidDesktopId,
    QueryWorkerFailed,
}

/// Sample of two independent documented IVirtualDesktopManager queries.
/// Missing portable metadata differs from this record's explicit failed native queries.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, uniffi::Record)]
#[serde(deny_unknown_fields)]
pub struct VirtualDesktopMembership {
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_guid_schema")]
    pub desktop_id: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_bool_schema")]
    pub on_current_desktop: Option<bool>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_error_schema")]
    pub desktop_id_error: Option<DesktopQueryError>,
    #[serde(deserialize_with = "required_nullable")]
    #[schemars(required, schema_with = "nullable_error_schema")]
    pub on_current_desktop_error: Option<DesktopQueryError>,
    /// Unix milliseconds when these membership queries completed, not when pixels were captured.
    pub sampled_at_unix_ms: u64,
}

fn required_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn nullable_bool_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":["boolean","null"]})
}
fn nullable_error_schema(g: &mut SchemaGenerator) -> Schema {
    Option::<DesktopQueryError>::json_schema(g)
}
fn nullable_guid_schema(_: &mut SchemaGenerator) -> Schema {
    json_schema!({"type":["string","null"],
        "pattern":"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
        "not":{"const":"00000000-0000-0000-0000-000000000000"}})
}

fn valid_guid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, b)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
        && value.bytes().any(|b| b != b'0' && b != b'-')
}

impl VirtualDesktopMembership {
    /// Build independent query outcomes; reject a malformed or zero GUID without erasing the other result.
    pub fn from_queries(
        desktop_id: Result<String, DesktopQueryError>,
        current: Result<bool, DesktopQueryError>,
        sampled_at_unix_ms: u64,
    ) -> Self {
        let desktop_id = desktop_id.and_then(|id| {
            if valid_guid(&id) {
                Ok(id)
            } else {
                Err(DesktopQueryError {
                    code: DesktopQueryErrorCode::InvalidDesktopId,
                    hresult: None,
                })
            }
        });
        let (desktop_id, desktop_id_error) = match desktop_id {
            Ok(id) => (Some(id), None),
            Err(error) => (None, Some(error)),
        };
        let (on_current_desktop, on_current_desktop_error) = match current {
            Ok(value) => (Some(value), None),
            Err(error) => (None, Some(error)),
        };
        Self {
            desktop_id,
            on_current_desktop,
            desktop_id_error,
            on_current_desktop_error,
            sampled_at_unix_ms,
        }
    }

    /// Validate that every value is either known or carries its failure, never both or neither.
    pub fn validate(&self) -> Result<(), String> {
        if self.desktop_id.is_some() == self.desktop_id_error.is_some() {
            return Err("desktop_id must contain one value or one query error".into());
        }
        if self.on_current_desktop.is_some() == self.on_current_desktop_error.is_some() {
            return Err("on_current_desktop must contain one value or one query error".into());
        }
        if self.desktop_id.as_deref().is_some_and(|id| !valid_guid(id)) {
            return Err("desktop_id must be a nonzero GUID".into());
        }
        Ok(())
    }
}
