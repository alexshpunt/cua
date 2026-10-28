use cua_driver_contract::*;
use serde_json::{json, Value};

fn input(operation: &str) -> Value {
    json!({"pid":42,"window_id":73,"state_id":"state","operation":operation})
}
fn sample() -> WindowControlSample {
    WindowControlSample {
        bounds: DesktopWindowBounds {
            x: -100,
            y: 20,
            width: 640,
            height: 480,
        },
        bounds_source: WindowBoundsSource::DwmVisibleFrame,
        coordinate_space: WindowManagementSpace::WindowsDesktopPixels,
        minimized: false,
        maximized: false,
        visible: true,
        foreground_at_target: false,
        keyboard_focus_at_target: None,
        foreground_window: 99,
        virtual_desktop: VirtualDesktopMembership::from_queries(
            Ok("12345678-1234-1234-1234-123456789abc".into()),
            Ok(true),
            1,
        ),
    }
}

#[test]
fn operation_shapes_and_integer_limits_are_enforced_without_input_authority() {
    for operation in ["minimize", "maximize", "focus"] {
        let valid = input(operation);
        serde_json::from_value::<WindowManagementInput>(valid.clone())
            .unwrap()
            .validate()
            .unwrap();
        for (field, value) in [
            ("x", json!(0)),
            ("y", json!(0)),
            ("width", json!(1)),
            ("height", json!(1)),
            ("unit", json!("windows_desktop_pixels")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<WindowManagementInput>(invalid)
                .unwrap()
                .validate()
                .is_err());
        }
    }
    for (operation, fields) in [
        (
            "move",
            json!({"x":-100000,"y":100000,"unit":"windows_desktop_pixels"}),
        ),
        (
            "resize",
            json!({"width":1,"height":32767,"unit":"windows_desktop_pixels"}),
        ),
    ] {
        let mut valid = input(operation);
        valid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_value::<WindowManagementInput>(valid.clone())
            .unwrap()
            .validate()
            .unwrap();
        for field in fields.as_object().unwrap().keys() {
            let mut missing = valid.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<WindowManagementInput>(missing)
                .unwrap()
                .validate()
                .is_err());
        }
        let mut mixed = valid.clone();
        mixed[if operation == "move" { "width" } else { "x" }] = json!(2);
        assert!(serde_json::from_value::<WindowManagementInput>(mixed)
            .unwrap()
            .validate()
            .is_err());
        let mut out_of_range = valid.clone();
        out_of_range[if operation == "move" { "x" } else { "width" }] = json!(100001);
        assert!(
            serde_json::from_value::<WindowManagementInput>(out_of_range)
                .unwrap()
                .validate()
                .is_err()
        );
        let mut fractional = valid.clone();
        fractional[if operation == "move" { "x" } else { "width" }] = json!(1.5);
        assert!(serde_json::from_value::<WindowManagementInput>(fractional).is_err());
    }
    for (field, value) in [
        ("pid", json!(0)),
        ("pid", json!(2147483648u64)),
        ("window_id", json!(0)),
        ("state_id", json!("  ")),
    ] {
        let mut invalid = input("focus");
        invalid[field] = value;
        assert!(serde_json::from_value::<WindowManagementInput>(invalid)
            .unwrap()
            .validate()
            .is_err());
    }
    for field in ["capture_id", "delivery_mode", "sequence", "token", "button"] {
        let mut invalid = input("focus");
        invalid[field] = json!("unexpected");
        assert!(serde_json::from_value::<WindowManagementInput>(invalid).is_err());
    }
    assert!(serde_json::from_value::<WindowManagementInput>(input("restore")).is_err());
    assert!(WindowControlStateInput {
        pid: 0,
        window_id: 73,
        session: None
    }
    .validate()
    .is_err());
}

#[test]
fn successful_receipts_require_exact_measured_postconditions_and_complete_prefixes() {
    let before = sample();
    let requested = DesktopWindowBounds {
        x: 200,
        y: 100,
        ..before.bounds
    };
    let mut observed = before.clone();
    observed.bounds = requested;
    let valid = ManagementOutput {
        pid: 42,
        window_id: 73,
        state_id: "state".into(),
        operation: WindowManagementOperation::Move,
        requested_bounds: Some(requested),
        mutation_attempted: true,
        mutation_accepted: true,
        postcondition_confirmed: true,
        before: Some(before.clone()),
        observed: Some(observed),
        route: WindowManagementRoute::Native,
        effect: WindowManagementEffect::Confirmed,
    };
    valid.validate().unwrap();
    let mut invalid = valid.clone();
    invalid.mutation_accepted = false;
    assert!(invalid.validate().is_err());
    let mut invalid = valid.clone();
    invalid.before = None;
    assert!(invalid.validate().is_err());
    let mut invalid = valid.clone();
    invalid.observed.as_mut().unwrap().bounds.x += 1;
    assert!(invalid.validate().is_err());
    let mut invalid = valid.clone();
    invalid.observed.as_mut().unwrap().bounds_source = WindowBoundsSource::WindowRect;
    assert!(invalid.validate().is_err());
    let mut invalid = valid.clone();
    invalid.requested_bounds.as_mut().unwrap().width += 1;
    invalid.observed.as_mut().unwrap().bounds.width += 1;
    assert!(invalid.validate().is_err());
    let mut invalid = valid.clone();
    invalid.operation = WindowManagementOperation::Focus;
    invalid.requested_bounds = None;
    assert!(invalid.validate().is_err());
    let mut focus = invalid;
    focus.observed.as_mut().unwrap().foreground_at_target = true;
    focus.observed.as_mut().unwrap().foreground_window = 73;
    focus.validate().unwrap();
    focus.observed.as_mut().unwrap().minimized = true;
    assert!(focus.validate().is_err());
    let state = WindowControlStateOutput {
        pid: 42,
        window_id: 73,
        state_id: "state".into(),
        state: before,
    };
    state.validate().unwrap();
    let mut invalid = state;
    invalid.state.foreground_at_target = true;
    assert!(invalid.validate().is_err());
    assert!(DesktopWindowBounds {
        x: i32::MAX,
        y: 0,
        width: 2,
        height: 1
    }
    .validate()
    .is_err());
    assert!(DesktopWindowBounds {
        x: 0,
        y: 0,
        width: 0,
        height: 1
    }
    .validate()
    .is_err());
    let mut missing = serde_json::to_value(sample()).unwrap();
    missing
        .as_object_mut()
        .unwrap()
        .remove("keyboard_focus_at_target");
    assert!(serde_json::from_value::<WindowControlSample>(missing).is_err());
}

#[test]
fn a_resize_receipt_allows_the_scaled_window_size_difference_and_nothing_else() {
    let before = sample();
    let requested = DesktopWindowBounds {
        width: before.bounds.width + 100,
        height: before.bounds.height + 50,
        ..before.bounds
    };
    let receipt = |dx: i64, dy: i64, shift: i32| {
        let mut observed = before.clone();
        observed.bounds = DesktopWindowBounds {
            x: requested.x + shift,
            width: (i64::from(requested.width) + dx) as u32,
            height: (i64::from(requested.height) + dy) as u32,
            ..requested
        };
        ManagementOutput {
            pid: 42,
            window_id: 73,
            state_id: "state".into(),
            operation: WindowManagementOperation::Resize,
            requested_bounds: Some(requested),
            mutation_attempted: true,
            mutation_accepted: true,
            postcondition_confirmed: true,
            before: Some(before.clone()),
            observed: Some(observed),
            route: WindowManagementRoute::Native,
            effect: WindowManagementEffect::Confirmed,
        }
    };
    let allowed = MAX_RESIZE_SIZE_DIFFERENCE_PX as i64;
    for (dx, dy) in [(0, 0), (1, 0), (-1, 0), (0, allowed), (-allowed, -allowed)] {
        receipt(dx, dy, 0).validate().unwrap();
    }
    for (dx, dy, shift) in [(allowed + 1, 0, 0), (0, -allowed - 1, 0), (0, 0, 1)] {
        assert!(
            receipt(dx, dy, shift).validate().is_err(),
            "{dx} {dy} {shift}"
        );
    }
    // A move never gets the allowance.
    let mut moved = receipt(1, 0, 0);
    moved.operation = WindowManagementOperation::Move;
    assert!(moved.validate().is_err());
}

#[test]
fn management_tools_are_windows_only_canonical_metadata_and_native_mutation() {
    let tools = manifest().tools;
    for (name, readonly) in [("get_window_control_state", true), ("manage_window", false)] {
        let tool = tools.iter().find(|tool| tool.name == name).unwrap();
        assert_eq!(tool.platforms, vec![Platform::Windows]);
        assert_eq!(tool.schema_mode, SchemaMode::CanonicalRuntime);
        assert_eq!(tool.annotations.read_only, readonly);
        assert!(!tool.annotations.idempotent);
        assert!(tool.cursor_semantics.is_none());
        assert!(tool.success_output_schema.is_some());
    }
}
