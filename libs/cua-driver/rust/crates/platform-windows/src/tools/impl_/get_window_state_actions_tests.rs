use super::*;
use crate::uia::{UiaNode, UiaStates};

fn node(actions: Vec<String>) -> UiaNode {
    UiaNode {
        element_index: Some(1),
        control_type: "Button".to_owned(),
        name: Some("OK".to_owned()),
        value: None,
        automation_id: None,
        help_text: None,
        actionable: !actions.is_empty(),
        actions,
        enabled: Some(true),
        selected: None,
        element_ptr: 0,
        center_x: 0,
        center_y: 0,
        rect: None,
        msaa_role: None,
        depth: 0,
        parent_element_index: None,
        in_web_content: false,
        states: UiaStates::default(),
    }
}

#[test]
fn element_entry_includes_actions_when_present() {
    let n = node(vec!["invoke".to_owned(), "toggle".to_owned()]);
    let entry = build_element_entry(&n, None).unwrap();
    assert_eq!(entry["actions"], json!(["invoke", "toggle"]));
    assert_eq!(entry["actionable"], json!(true));
}

#[test]
fn element_entry_omits_actions_when_empty() {
    let n = node(Vec::new());
    let entry = build_element_entry(&n, None).unwrap();
    assert!(entry.get("actions").is_none());
    assert_eq!(entry["actionable"], json!(false));
}

#[test]
fn read_only_rows_keep_index_token_frame_and_parent() {
    let mut n = node(Vec::new());
    n.control_type = "Text".to_owned();
    n.name = Some("idle".to_owned());
    n.enabled = Some(true);
    n.rect = Some((10, 20, 110, 40));
    n.parent_element_index = Some(0);
    let entry = build_element_entry(&n, Some(7)).unwrap();
    assert_eq!(entry["element_index"], json!(1));
    assert!(entry["element_token"].as_str().unwrap().ends_with(":1"));
    assert_eq!(
        entry["frame"],
        json!({ "x": 10, "y": 20, "w": 100, "h": 20 })
    );
    assert_eq!(entry["parent_index"], json!(0));
    assert_eq!(entry["label"], json!("idle"));
    assert_eq!(entry["name"], json!("idle"));
}

#[test]
fn structural_states_appear_only_when_reported() {
    let mut n = node(vec!["toggle".to_owned()]);
    n.automation_id = Some("agree".to_owned());
    n.states = UiaStates {
        class_name: Some("WindowsForms10.BUTTON".to_owned()),
        checked: Some("mixed"),
        expanded: Some(false),
        offscreen: Some(false),
        focused: Some(true),
        password: Some(false),
        read_only: Some(true),
        required: Some(false),
        range: Some((5.0, 0.0, 10.0)),
    };
    let entry = build_element_entry(&n, None).unwrap();
    assert_eq!(entry["automation_id"], json!("agree"));
    assert_eq!(entry["class_name"], json!("WindowsForms10.BUTTON"));
    assert_eq!(entry["checked"], json!("mixed"));
    assert_eq!(entry["expanded"], json!(false));
    assert_eq!(entry["offscreen"], json!(false));
    assert_eq!(entry["focused"], json!(true));
    assert_eq!(entry["read_only"], json!(true));
    assert_eq!(
        entry["range"],
        json!({ "value": 5.0, "min": 0.0, "max": 10.0 })
    );
    // Rare flags appear only when true; an unreported state is absent, not false.
    assert!(entry.get("password").is_none());
    assert!(entry.get("required").is_none());

    let bare = build_element_entry(&node(Vec::new()), None).unwrap();
    for field in [
        "checked",
        "expanded",
        "offscreen",
        "focused",
        "read_only",
        "range",
        "class_name",
        "automation_id",
    ] {
        assert!(
            bare.get(field).is_none(),
            "{field} must be omitted when UIA did not report it"
        );
    }
}
