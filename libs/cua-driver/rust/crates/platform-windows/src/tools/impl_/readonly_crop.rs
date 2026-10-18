//! The explicit WGC zoom branch is readonly and never touches snapshot contexts.

use super::{ToolResult, ToolState};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cua_driver_core::protocol::Content;
use serde_json::{json, Value};

pub(super) fn refusal(code: &'static str) -> ToolResult {
    ToolResult::error(code).with_structured(json!({
        "code":code,"effect":"refused","input_authority":false
    }))
}

pub(super) async fn invoke(state: &ToolState, args: &Value) -> ToolResult {
    let (target, region) = match crate::wgc_crop::request(args) {
        Ok(request) => request,
        Err(code) => return refusal(code),
    };
    let frame = match state.persistent_wgc.crop(target, region).await {
        Ok(frame) => frame,
        Err(_) => return refusal("wgc_crop_capture_unavailable"),
    };
    ToolResult {
        content:vec![Content::image_png(B64.encode(frame.png)), Content::text(
            "Captured a lossless readonly WGC crop. Application paint freshness is unverified. This image grants no input authority."
        )],
        is_error:None,
        structured_content:Some(frame.metadata),
        action_record:None,
    }
}
