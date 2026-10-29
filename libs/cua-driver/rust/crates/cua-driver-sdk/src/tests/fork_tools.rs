//! Exercise fork methods through the public authenticated carrier and session APIs.
use super::*;
use crate::remote::{
    DriverChannelCapabilities, DriverEnvelopeChannel, DriverRequestEnvelope,
    DriverResponseEnvelope, DRIVER_ENVELOPE_VERSION,
};
use serde_json::json;
use std::sync::Mutex;

#[derive(Clone)]
struct RecordingChannel {
    requests: Arc<Mutex<Vec<(bool, DriverRequestEnvelope)>>>,
    result: Value,
    bound: bool,
}
impl RecordingChannel {
    fn new(result: Value) -> Self {
        Self {
            requests: Arc::new(Mutex::new(vec![])),
            result,
            bound: false,
        }
    }
}
#[async_trait::async_trait]
impl DriverEnvelopeChannel for RecordingChannel {
    async fn negotiate(&self) -> Result<DriverChannelCapabilities, String> {
        Ok(DriverChannelCapabilities {
            minimum_envelope_version: DRIVER_ENVELOPE_VERSION,
            maximum_envelope_version: DRIVER_ENVELOPE_VERSION,
            supports_cancellation: true,
        })
    }
    async fn exchange(
        &self,
        request: DriverRequestEnvelope,
    ) -> Result<DriverResponseEnvelope, String> {
        assert_eq!(request.operation, "call");
        let id = request.request_id.clone();
        self.requests.lock().unwrap().push((self.bound, request));
        Ok(DriverResponseEnvelope {
            envelope_version: DRIVER_ENVELOPE_VERSION,
            request_id: id,
            ok: true,
            result: Some(self.result.clone()),
            error: None,
            error_code: None,
            completion_known: true,
        })
    }
    async fn bind_session(
        &self,
        _: TrustedSessionOptions,
    ) -> Result<Arc<dyn DriverEnvelopeChannel>, String> {
        Ok(Arc::new(Self {
            bound: true,
            ..self.clone()
        }))
    }
    async fn close(&self) -> Result<(), String> {
        Ok(())
    }
    async fn cancel(&self, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn authenticated_principal(&self) -> &str {
        "sdk-contract-fixture"
    }
    fn connection_generation(&self) -> &str {
        "sdk-contract-generation"
    }
}
fn bound_options() -> TrustedSessionOptions {
    TrustedSessionOptions {
        public_session: "sdk-bound".into(),
        mode: SessionPermissionMode::Standard,
        ttl_seconds: 60,
        idle_ttl_seconds: 30,
        capability_manifest_path: None,
        bounded_manifest_path: None,
    }
}
macro_rules! fork_inputs {
    ($check:ident) => {
        $check!(list_displays, ListDisplaysInput, json!({"session":"sdk-call"}));
        $check!(get_display_state, GetDisplayStateInput, json!({"display_id":"display-1","topology_id":"topology-1","session":"sdk-call","max_image_dimension":400}));
        $check!(get_window_status, WindowStatusInput, json!({"pid":42,"window_id":73,"session":"sdk-call"}));
        $check!(restore_window, WindowRestoreInput, json!({"pid":42,"window_id":73,"state_id":"restore-1","session":"sdk-call"}));
        $check!(get_window_control_state, WindowControlStateInput, json!({"pid":42,"window_id":73,"session":"sdk-call"}));
        $check!(manage_window, WindowManagementInput, json!({"pid":42,"window_id":73,"state_id":"control-1","operation":"resize","width":640,"height":480,"unit":"windows_desktop_pixels","session":"sdk-call"}));
        $check!(move_pointer, PointerMoveInput, json!({"pid":42,"window_id":73,"capture_id":"capture-1","x":10.0,"y":20.0,"dwell_ms":0,"delivery_mode":"foreground","session":"sdk-call"}));
        $check!(scroll_at, PointerScrollInput, json!({"pid":42,"window_id":73,"capture_id":"capture-1","x":10.0,"y":20.0,"delta_x":0,"delta_y":1,"unit":"wheel_detents","delivery_mode":"foreground","session":"sdk-call"}));
        $check!(semantic_action, SemanticActionInput, json!({"pid":42,"window_id":73,"element_token":"element-1","operation":"invoke","session":"sdk-call"}));
        $check!(set_agent_visuals, SetAgentVisualsInput, json!({"preset":"off","opacity":0.5,"session":"sdk-call"}));
        $check!(present_agent_activity, PresentAgentActivityInput, json!({"generation":1,"kind":"clear","session":"sdk-call"}));
    }
}

#[tokio::test]
async fn all_fork_methods_route_typed_inputs_through_driver_and_bound_session() {
    let channel = RecordingChannel::new(
        json!({"content":[],"isError":true,"structuredContent":{"code":"fixture_refusal","effect":"refused"}}),
    );
    let driver = CuaDriver::connect_remote(Arc::new(channel.clone())).unwrap();
    let session = driver
        .create_remote_trusted_session(bound_options())
        .await
        .unwrap();
    macro_rules! check {
        ($method:ident,$input:ty,$args:expr) => {{
            let args = $args;
            let result = driver
                .$method(serde_json::from_value::<$input>(args.clone()).unwrap())
                .await
                .unwrap();
            assert!(result.is_error);
            assert_eq!(result.error_code.as_deref(), Some("fixture_refusal"));
            let result = session
                .$method(serde_json::from_value::<$input>(args.clone()).unwrap())
                .await
                .unwrap();
            assert!(result.is_error);
            assert_eq!(result.error_code.as_deref(), Some("fixture_refusal"));
            let requests = channel.requests.lock().unwrap();
            let pair = &requests[requests.len() - 2..];
            for ((bound, request), expected_bound) in pair.iter().zip([false, true]) {
                assert_eq!(*bound, expected_bound);
                assert_eq!(request.name.as_deref(), Some(stringify!($method)));
                assert_eq!(request.arguments.as_ref(), Some(&args));
                assert!(!request.request_id.is_empty());
            }
        }};
    }
    fork_inputs!(check);
    assert_eq!(channel.requests.lock().unwrap().len(), 22);
    session.close();
    driver.shutdown().await.unwrap();
}

#[tokio::test]
async fn fork_methods_reject_malformed_successes_from_authenticated_transports() {
    let channel =
        RecordingChannel::new(json!({"content":[],"isError":false,"structuredContent":{}}));
    let driver = CuaDriver::connect_remote(Arc::new(channel.clone())).unwrap();
    let session = driver
        .create_remote_trusted_session(bound_options())
        .await
        .unwrap();
    macro_rules! check {
        ($method:ident,$input:ty,$args:expr) => {{
            let args = $args;
            assert!(matches!(
                driver
                    .$method(serde_json::from_value::<$input>(args.clone()).unwrap())
                    .await,
                Err(DriverError::Protocol { .. })
            ));
            assert!(matches!(
                session
                    .$method(serde_json::from_value::<$input>(args).unwrap())
                    .await,
                Err(DriverError::Protocol { .. })
            ));
        }};
    }
    fork_inputs!(check);
    assert_eq!(channel.requests.lock().unwrap().len(), 22);
    session.close();
    driver.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_fork_requests_fail_before_driver_or_session_transport() {
    let channel = RecordingChannel::new(
        json!({"content":[],"isError":true,"structuredContent":{"code":"fixture_refusal"}}),
    );
    let driver = CuaDriver::connect_remote(Arc::new(channel.clone())).unwrap();
    let session = driver
        .create_remote_trusted_session(bound_options())
        .await
        .unwrap();
    macro_rules! invalid {
        ($method:ident,$input:ty,$args:expr) => {{
            let args=$args;
            for result in [driver.$method(serde_json::from_value::<$input>(args.clone()).unwrap()).await,session.$method(serde_json::from_value::<$input>(args).unwrap()).await] {
                assert!(matches!(result,Err(DriverError::InvalidArguments { tool, .. }) if tool==stringify!($method)));
            }
        }};
    }
    invalid!(
        get_display_state,
        GetDisplayStateInput,
        json!({"display_id":"","topology_id":"topology-1"})
    );
    invalid!(
        get_window_status,
        WindowStatusInput,
        json!({"pid":0,"window_id":73})
    );
    invalid!(
        restore_window,
        WindowRestoreInput,
        json!({"pid":42,"window_id":73,"state_id":""})
    );
    invalid!(
        get_window_control_state,
        WindowControlStateInput,
        json!({"pid":0,"window_id":73})
    );
    invalid!(
        manage_window,
        WindowManagementInput,
        json!({"pid":42,"window_id":73,"state_id":"control-1","operation":"resize","width":0,"height":480,"unit":"windows_desktop_pixels"})
    );
    invalid!(
        move_pointer,
        PointerMoveInput,
        json!({"pid":42,"window_id":73,"capture_id":"capture-1","x":10.0,"y":20.0,"dwell_ms":10001,"delivery_mode":"foreground"})
    );
    invalid!(
        scroll_at,
        PointerScrollInput,
        json!({"pid":42,"window_id":73,"capture_id":"capture-1","x":10.0,"y":20.0,"delta_x":0,"delta_y":0,"unit":"wheel_detents","delivery_mode":"foreground"})
    );
    invalid!(
        semantic_action,
        SemanticActionInput,
        json!({"pid":42,"window_id":73,"element_token":"element-1","operation":"set_value"})
    );
    invalid!(
        set_agent_visuals,
        SetAgentVisualsInput,
        json!({"opacity":1.5})
    );
    invalid!(
        present_agent_activity,
        PresentAgentActivityInput,
        json!({"generation":0,"kind":"clear"})
    );
    assert!(channel.requests.lock().unwrap().is_empty());
    session.close();
    driver.shutdown().await.unwrap();
}

#[tokio::test]
async fn visual_methods_preserve_validated_admission_receipts_without_inventing_delivery() {
    let configured = json!({"status":"configured","settings":{"preset":"off","observations":true,"previews":true,"pointer":true,"scroll_drag":true,"keyboard":true,"opacity":0.5,"quality":"balanced","reduced_motion":false}});
    let channel =
        RecordingChannel::new(json!({"content":[],"isError":false,"structuredContent":configured}));
    let driver = CuaDriver::connect_remote(Arc::new(channel)).unwrap();
    let result = driver
        .set_agent_visuals(serde_json::from_value(json!({"preset":"off"})).unwrap())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(result.structured_json.as_deref().unwrap()).unwrap(),
        configured
    );
    driver.shutdown().await.unwrap();
    for status in ["queued", "cleared", "disabled", "stale"] {
        let ack = json!({"status":status,"generation":1});
        let channel =
            RecordingChannel::new(json!({"content":[],"isError":false,"structuredContent":ack}));
        let driver = CuaDriver::connect_remote(Arc::new(channel)).unwrap();
        let result = driver
            .present_agent_activity(
                serde_json::from_value(json!({"generation":1,"kind":"clear"})).unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(result.structured_json.as_deref().unwrap()).unwrap(),
            ack
        );
        assert!(result.action.is_none());
        assert!(result.images.is_empty());
        driver.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn typed_preview_transport_preserves_source_geometry_and_image_bytes() {
    let args = json!({"generation":2,"kind":"overview","source":{"display":"display-1","bounds":{"x":-20,"y":10,"width":80,"height":40}},"region":{"x":-10,"y":12,"width":10,"height":8},"point":{"x":-9,"y":13},"image":{"data":"YWJj","mime_type":"image/png"},"action":"observe","outcome":"verified"});
    let channel = RecordingChannel::new(
        json!({"content":[],"isError":false,"structuredContent":{"status":"queued","generation":2}}),
    );
    let driver = CuaDriver::connect_remote(Arc::new(channel.clone())).unwrap();
    let input: PresentAgentActivityInput = serde_json::from_value(args.clone()).unwrap();
    driver.present_agent_activity(input).await.unwrap();
    let requests = channel.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1.name.as_deref(),
        Some("present_agent_activity")
    );
    assert_eq!(requests[0].1.arguments.as_ref(), Some(&args));
    drop(requests);
    driver.shutdown().await.unwrap();
}

#[tokio::test]
async fn unsafe_preview_metadata_is_rejected_without_sending_image_bytes() {
    let base = json!({"generation":2,"kind":"overview","source":{"display":"display-1","bounds":{"x":0,"y":0,"width":20,"height":20}},"image":{"data":"PRIVATE_IMAGE_SENTINEL","mime_type":"image/png"}});
    let channel = RecordingChannel::new(
        json!({"content":[],"isError":true,"structuredContent":{"code":"fixture_refusal"}}),
    );
    let driver = CuaDriver::connect_remote(Arc::new(channel.clone())).unwrap();
    let mut inputs = vec![];
    let mut changed = base.clone();
    changed["region"] = json!({"x":19,"y":0,"width":2,"height":1});
    inputs.push(changed);
    let mut changed = base.clone();
    changed["point"] = json!({"x":20,"y":0});
    inputs.push(changed);
    let mut changed = base.clone();
    changed["kind"] = json!("action");
    inputs.push(changed);
    let mut changed = base.clone();
    changed["kind"] = json!("clear");
    inputs.push(changed);
    let mut changed = base.clone();
    changed["outcome"] = json!("refused");
    inputs.push(changed);
    for value in inputs {
        let input: PresentAgentActivityInput = serde_json::from_value(value).unwrap();
        assert!(
            matches!(driver.present_agent_activity(input).await,Err(DriverError::InvalidArguments { tool, .. }) if tool=="present_agent_activity")
        );
    }
    assert!(channel.requests.lock().unwrap().is_empty());
    driver.shutdown().await.unwrap();
}

#[test]
fn visual_inputs_keep_the_published_schema_and_reject_mixed_source_authority() {
    assert_eq!(
        SetAgentVisualsInput::input_schema(),
        cua_driver_contract::tool_contract("set_agent_visuals")
            .unwrap()
            .input_schema
    );
    assert_eq!(
        PresentAgentActivityInput::input_schema(),
        cua_driver_contract::tool_contract("present_agent_activity")
            .unwrap()
            .input_schema
    );
    let mixed = json!({"generation":1,"kind":"overview","source":{"window_id":73,"pid":42,"identity":"PiComputerUse.00000000000000000000000000000001:42:1","display":"display-1","bounds":{"x":0,"y":0,"width":20,"height":20}}});
    assert!(serde_json::from_value::<PresentAgentActivityInput>(mixed).is_err());
    assert_eq!(
        serde_json::to_value(SetAgentVisualsInput::default()).unwrap(),
        json!({})
    );
}
