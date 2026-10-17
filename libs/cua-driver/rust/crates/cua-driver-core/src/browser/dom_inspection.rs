//! Bounded, read-only DOM records. Native CDP frame and layout proof stays with the caller.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

/// One collected element; raw CDP identities remain internal to the browser engine.
#[derive(Debug, Clone)]
pub(crate) struct DomRecord {
    pub(crate) backend_node_id: i64,
    pub(crate) node_id: i64,
    pub(crate) parent_backend_node_id: Option<i64>,
    pub(crate) frame_key: String,
    pub(crate) tag: String,
    pub(crate) attributes: BTreeMap<String, String>,
    pub(crate) text: String,
    pub(crate) role: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) value: Option<String>,
    pub(crate) states: BTreeMap<String, Value>,
    pub(crate) visibility: String,
    pub(crate) text_truncated: bool,
}

/// Exact semantic predicates, substring text, and native-resolved CSS; predicates combine with AND.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DomLocator {
    pub(crate) role: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) test_id: Option<String>,
    pub(crate) css: Option<String>,
}

/// Coverage excludes browser-owned internals, not authored closed shadow trees.
pub(crate) const COLLECTION_SCOPE: &str = "author_dom_with_open_shadow";

/// Text-free, fixed host buckets; browser-generated names and identities are never exported.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct UserAgentShadowHosts {
    input: usize,
    select: usize,
    option: usize,
    textarea: usize,
    button: usize,
    other: usize,
}

/// Explicit excluded scope. Each counted root consumes one collection visit;
/// its browser-owned descendants consume neither author depth nor node budget.
#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct DomExclusions {
    user_agent_shadow_roots: usize,
    user_agent_shadow_hosts: UserAgentShadowHosts,
}
impl DomExclusions {
    fn record(&mut self, host: &str) {
        self.user_agent_shadow_roots += 1;
        let hosts = &mut self.user_agent_shadow_hosts;
        let bucket = match host {
            tag if tag.eq_ignore_ascii_case("input") => &mut hosts.input,
            tag if tag.eq_ignore_ascii_case("select") => &mut hosts.select,
            tag if tag.eq_ignore_ascii_case("option") => &mut hosts.option,
            tag if tag.eq_ignore_ascii_case("textarea") => &mut hosts.textarea,
            tag if tag.eq_ignore_ascii_case("button") => &mut hosts.button,
            _ => &mut hosts.other,
        };
        *bucket += 1;
    }

    pub(crate) fn merge(&mut self, other: &Self) {
        self.user_agent_shadow_roots += other.user_agent_shadow_roots;
        let hosts = &mut self.user_agent_shadow_hosts;
        let added = &other.user_agent_shadow_hosts;
        hosts.input += added.input;
        hosts.select += added.select;
        hosts.option += added.option;
        hosts.textarea += added.textarea;
        hosts.button += added.button;
        hosts.other += added.other;
    }
}

/// Only Chromium's explicit native UA discriminator excludes a tree. Missing,
/// malformed and authored closed roots retain ordinary incomplete coverage.
pub(crate) fn is_user_agent_shadow_root(node: &Value) -> bool {
    node["nodeType"] == 11 && node["shadowRootType"] == "user-agent"
}
/// Omission counters are known lower bounds, not a guessed count of unseen descendants.
#[derive(Debug, Clone, Default)]
pub(crate) struct DomCoverage {
    pub(crate) complete: bool,
    pub(crate) visited: usize,
    pub(crate) depth_omitted: usize,
    pub(crate) node_omitted: usize,
    pub(crate) text_truncated: usize,
    pub(crate) excluded: DomExclusions,
}

const MAX_NODES: usize = 2_000;
const MAX_DEPTH: usize = 64;
const MAX_TEXT: usize = 1_024;
const MAX_ATTRIBUTES: usize = 64;

fn bounded(value: &str) -> (String, bool) {
    let mut chars = value.chars();
    let text = chars.by_ref().take(MAX_TEXT).collect();
    (text, chars.next().is_some())
}

struct Collector {
    rows: Vec<DomRecord>,
    coverage: DomCoverage,
    max_nodes: usize,
    max_depth: usize,
    next_frame: usize,
}

impl Collector {
    fn omitted(&mut self, count: usize) {
        self.coverage.complete = false;
        self.coverage.node_omitted = self.coverage.node_omitted.saturating_add(count);
    }

    fn skip_remaining(&mut self, depth: usize, count: usize) -> bool {
        if count == 0 {
            return false;
        }
        if depth > self.max_depth {
            self.coverage.complete = false;
            self.coverage.depth_omitted = self.coverage.depth_omitted.saturating_add(count);
            return true;
        }
        if self.coverage.visited >= self.max_nodes {
            self.omitted(count);
            return true;
        }
        false
    }
    fn attributes(&mut self, node: &Value) -> (BTreeMap<String, String>, bool) {
        let mut attributes = BTreeMap::new();
        let Some(raw) = node.get("attributes") else {
            return (attributes, false);
        };
        let Some(raw) = raw.as_array() else {
            self.omitted(1);
            return (attributes, true);
        };
        let mut truncated = raw.len() % 2 != 0 || raw.len() / 2 > MAX_ATTRIBUTES;
        for pair in raw.chunks_exact(2).take(MAX_ATTRIBUTES) {
            let (Some(key), Some(value)) = (pair[0].as_str(), pair[1].as_str()) else {
                truncated = true;
                continue;
            };
            let (key, key_truncated) = bounded(key);
            let (value, value_truncated) = bounded(value);
            truncated |= key_truncated || value_truncated;
            if attributes.insert(key, value).is_some() {
                truncated = true;
            }
        }
        (attributes, truncated)
    }

    fn append_text(&mut self, ancestors: &[usize], value: &str) {
        for index in ancestors {
            let row = &mut self.rows[*index];
            let remaining = MAX_TEXT - row.text.chars().count();
            let mut chars = value.chars();
            row.text.extend(chars.by_ref().take(remaining));
            row.text_truncated |= chars.next().is_some();
        }
    }

    fn walk(&mut self, node: &Value, frame: &str, depth: usize, ancestors: &[usize]) {
        if depth > self.max_depth {
            self.coverage.complete = false;
            self.coverage.depth_omitted += 1;
            return;
        }
        if self.coverage.visited >= self.max_nodes {
            self.omitted(1);
            return;
        }
        self.coverage.visited += 1;
        let Some(kind) = node.get("nodeType").and_then(Value::as_i64) else {
            self.omitted(1);
            return;
        };
        if kind == 3 {
            match node.get("nodeValue").and_then(Value::as_str) {
                Some(value) => self.append_text(ancestors, value),
                None => self.omitted(1),
            }
        }
        let mut ancestors = ancestors.to_vec();
        if kind == 1 {
            let (Some(backend), Some(frontend), Some(tag)) = (
                node.get("backendNodeId")
                    .and_then(Value::as_i64)
                    .filter(|id| *id > 0),
                node.get("nodeId")
                    .and_then(Value::as_i64)
                    .filter(|id| *id > 0),
                node.get("nodeName").and_then(Value::as_str),
            ) else {
                self.omitted(1);
                return;
            };
            let (attributes, attrs_truncated) = self.attributes(node);
            let (tag, tag_truncated) = bounded(tag);
            let parent = ancestors
                .last()
                .map(|index| self.rows[*index].backend_node_id);
            let row = DomRecord {
                backend_node_id: backend,
                node_id: frontend,
                parent_backend_node_id: parent,
                frame_key: frame.to_owned(),
                tag: tag.to_ascii_lowercase(),
                role: attributes.get("role").cloned(),
                name: attributes.get("aria-label").cloned(),
                // Markup values, checked attributes and inline style are not proof of live properties.
                value: None,
                states: BTreeMap::new(),
                visibility: "unknown".to_owned(),
                text: String::new(),
                text_truncated: attrs_truncated || tag_truncated,
                attributes,
            };
            ancestors.push(self.rows.len());
            self.rows.push(row);
        }

        let children = node.get("children").and_then(Value::as_array);
        let count = node.get("childNodeCount").and_then(Value::as_u64);
        if let Some(count) = count {
            let available = children.map_or(0, Vec::len);
            if count > available as u64 {
                self.omitted(usize::try_from(count - available as u64).unwrap_or(usize::MAX));
            } else if count < available as u64 {
                self.omitted(1);
            }
        } else if matches!(kind, 1 | 9 | 11) {
            // Missing child coverage is not evidence that this container is a leaf.
            self.omitted(1);
        }
        if node.get("children").is_some() && children.is_none() {
            self.omitted(1);
        }
        if let Some(children) = children {
            for (index, child) in children.iter().enumerate() {
                if self.skip_remaining(depth + 1, children.len() - index) {
                    break;
                }
                self.walk(child, frame, depth + 1, &ancestors);
            }
        }
        if let Some(roots) = node.get("shadowRoots") {
            if let Some(roots) = roots.as_array() {
                for (index, root) in roots.iter().enumerate() {
                    if is_user_agent_shadow_root(root) {
                        if self.coverage.visited >= self.max_nodes {
                            self.omitted(roots.len() - index);
                            break;
                        }
                        self.coverage.visited += 1;
                        self.coverage
                            .excluded
                            .record(node["nodeName"].as_str().unwrap_or(""));
                        continue;
                    }
                    if self.skip_remaining(depth + 1, roots.len() - index) {
                        break;
                    }
                    if root.get("shadowRootType").and_then(Value::as_str) == Some("open") {
                        self.walk(root, frame, depth + 1, &ancestors);
                    } else {
                        self.coverage.visited += 1;
                        self.omitted(1);
                    }
                }
            } else {
                self.omitted(1);
            }
        }
        if let Some(document) = node.get("contentDocument") {
            self.next_frame += 1;
            let child_frame = format!("{frame}/{}", self.next_frame);
            self.walk(document, &child_frame, depth + 1, &[]);
        } else if node
            .get("nodeName")
            .and_then(Value::as_str)
            .is_some_and(|tag| {
                tag.eq_ignore_ascii_case("iframe") || tag.eq_ignore_ascii_case("frame")
            })
        {
            // An OOPIF/unavailable frame may be supplied separately by the native collector.
            self.omitted(1);
        }
    }
}

/// Collect at most 2,000 visited DOM nodes and depth 1..64. Invalid budgets fail closed.
/// Text is bounded to 1,024 Unicode scalar values and attributes to 64 pairs per element.
/// Nested documents receive distinct local source keys; the caller must attest their real frames.
/// Completeness describes this DOM response only, never native paint or independently attached frames.
pub(crate) fn collect(
    document: &Value,
    frame_key: &str,
    max_nodes: usize,
    max_depth: usize,
) -> (Vec<DomRecord>, DomCoverage) {
    if !(1..=MAX_NODES).contains(&max_nodes)
        || !(1..=MAX_DEPTH).contains(&max_depth)
        || frame_key.is_empty()
        || frame_key.chars().take(129).count() > 128
    {
        return (
            Vec::new(),
            DomCoverage {
                node_omitted: 1,
                ..Default::default()
            },
        );
    }
    let mut collector = Collector {
        rows: Vec::new(),
        coverage: DomCoverage {
            complete: true,
            ..Default::default()
        },
        max_nodes,
        max_depth,
        next_frame: 0,
    };
    collector.walk(document.get("root").unwrap_or(document), frame_key, 0, &[]);
    collector.coverage.text_truncated = collector
        .rows
        .iter()
        .filter(|row| row.text_truncated)
        .count();
    collector.coverage.complete &= collector.coverage.text_truncated == 0;
    (collector.rows, collector.coverage)
}

/// Project source-qualified records, retaining duplicates and collection order.
/// Exact role/name/label/test-ID matching is case-sensitive; text is a case-sensitive substring.
/// Label means the reported accessible name; test ID means the `data-testid` attribute.
/// CSS is resolved only by a caller-supplied native match set keyed by frame and backend identity.
/// Projection limits do not prove exhaustive absence; callers retain collection and match coverage.
pub(crate) fn project(
    records: &[DomRecord],
    locator: &DomLocator,
    scope: Option<(&str, i64)>,
    css_matches: Option<&HashSet<(String, i64)>>,
    limit: usize,
) -> Result<Vec<DomRecord>, String> {
    if !(1..=MAX_NODES).contains(&limit) {
        return Err("dom_projection_limit_invalid".into());
    }
    if records.len() > MAX_NODES {
        return Err("dom_records_budget_exceeded".into());
    }
    for value in [
        &locator.role,
        &locator.name,
        &locator.label,
        &locator.text,
        &locator.test_id,
        &locator.css,
    ]
    .into_iter()
    .flatten()
    {
        if value.chars().take(257).count() > 256 || value.trim().is_empty() || value.contains('\0')
        {
            return Err("dom_locator_invalid".into());
        }
    }
    if locator.css.is_some() && css_matches.is_none() {
        return Err("dom_css_resolution_required".into());
    }
    let mut allowed = HashSet::new();
    if let Some((frame, backend)) = scope {
        let count = records
            .iter()
            .filter(|row| row.frame_key == frame && row.backend_node_id == backend)
            .count();
        match count {
            0 => return Err("dom_scope_not_found".into()),
            1 => {}
            _ => return Err("dom_scope_ambiguous".into()),
        }
        let mut identities = HashSet::new();
        for row in records.iter().filter(|row| row.frame_key == frame) {
            if !identities.insert(row.backend_node_id) {
                return Err("dom_scope_ambiguous".into());
            }
        }
        allowed.insert(backend);
        loop {
            let before = allowed.len();
            for row in records.iter().filter(|row| row.frame_key == frame) {
                if row
                    .parent_backend_node_id
                    .is_some_and(|parent| allowed.contains(&parent))
                {
                    allowed.insert(row.backend_node_id);
                }
            }
            if allowed.len() == before {
                break;
            }
        }
    }
    let rows = records
        .iter()
        .filter(|row| {
            scope.is_none_or(|(frame, _)| {
                row.frame_key == frame && allowed.contains(&row.backend_node_id)
            }) && locator
                .role
                .as_ref()
                .is_none_or(|value| row.role.as_ref() == Some(value))
                && locator
                    .name
                    .as_ref()
                    .is_none_or(|value| row.name.as_ref() == Some(value))
                && locator
                    .label
                    .as_ref()
                    .is_none_or(|value| row.name.as_ref() == Some(value))
                && locator
                    .text
                    .as_ref()
                    .is_none_or(|value| row.text.contains(value))
                && locator
                    .test_id
                    .as_ref()
                    .is_none_or(|value| row.attributes.get("data-testid") == Some(value))
                && (locator.css.is_none()
                    || css_matches.is_some_and(|matches| {
                        matches.contains(&(row.frame_key.clone(), row.backend_node_id))
                    }))
        })
        .take(limit)
        .cloned()
        .collect();
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn element(id: i64, tag: &str, attrs: Value, children: Vec<Value>) -> Value {
        json!({"nodeType":1,"nodeId":id,"backendNodeId":id,"nodeName":tag,
            "attributes":attrs,"childNodeCount":children.len(),"children":children})
    }
    fn text(id: i64, value: &str) -> Value {
        json!({"nodeType":3,"nodeId":id,"backendNodeId":id,"nodeValue":value})
    }
    fn document(children: Vec<Value>) -> Value {
        json!({"root":{"nodeType":9,"nodeId":100,"backendNodeId":100,
            "childNodeCount":children.len(),"children":children}})
    }
    fn records() -> Vec<DomRecord> {
        collect(
            &document(vec![element(
                1,
                "DIV",
                json!([]),
                vec![
                    element(
                        2,
                        "BUTTON",
                        json!([
                            "role",
                            "button",
                            "aria-label",
                            "Save",
                            "data-testid",
                            "save"
                        ]),
                        vec![text(20, "Save draft")],
                    ),
                    element(
                        3,
                        "BUTTON",
                        json!(["role", "button", "aria-label", "Save"]),
                        vec![],
                    ),
                    element(
                        4,
                        "BUTTON",
                        json!(["role", "buttonish", "aria-label", "Save as"]),
                        vec![],
                    ),
                ],
            )]),
            "frame-main",
            100,
            20,
        )
        .0
    }

    #[test]
    fn wide_omitted_branches_stop_at_the_visit_and_depth_budgets() {
        let children: Vec<Value> = (1..=5_000)
            .map(|id| element(id, "DIV", json!([]), vec![]))
            .collect();
        let (rows, coverage) = collect(&document(children), "main", 2, 4);
        assert_eq!(rows.len(), 1);
        assert_eq!(coverage.visited, 2);
        assert_eq!(coverage.node_omitted, 4_999);
        assert!(!coverage.complete);

        let roots = vec![json!({"nodeType":11,"shadowRootType":"closed"}); 5_000];
        let mut host = element(1, "DIV", json!([]), vec![]);
        host["shadowRoots"] = json!(roots);
        let (_, coverage) = collect(&document(vec![host.clone()]), "main", 3, 4);
        assert_eq!(coverage.visited, 3);
        assert_eq!(coverage.node_omitted, 5_000);
        let (_, coverage) = collect(&document(vec![host]), "main", 3, 1);
        assert_eq!(coverage.visited, 2);
        assert_eq!(coverage.depth_omitted, 5_000);
    }

    #[test]
    fn includes_hidden_and_noninteractive_elements_without_inventing_live_states_or_pixels() {
        let (rows, coverage) = collect(
            &document(vec![
                element(
                    1,
                    "DIV",
                    json!(["hidden", "", "data-testid", "secret"]),
                    vec![text(10, "Hidden detail")],
                ),
                element(
                    2,
                    "INPUT",
                    json!(["value", "markup", "checked", "", "disabled", ""]),
                    vec![],
                ),
            ]),
            "opaque-main",
            100,
            20,
        );
        assert_eq!(rows.len(), 2);
        assert!(rows[0].attributes.contains_key("hidden"));
        assert_eq!(rows[0].text, "Hidden detail");
        assert_eq!(
            rows[1].attributes.get("value").map(String::as_str),
            Some("markup")
        );
        assert!(rows[1].value.is_none());
        assert!(rows[1].states.is_empty());
        assert_eq!(rows[1].visibility, "unknown");
        assert!(coverage.complete);
    }

    #[test]
    fn open_shadow_roots_preserve_host_ancestry_but_nested_documents_get_distinct_sources() {
        let mut host = element(1, "DIV", json!([]), vec![]);
        host["shadowRoots"] = json!([{"nodeType":11,"nodeId":50,"backendNodeId":50,"shadowRootType":"open",
            "childNodeCount":1,"children":[element(2,"BUTTON",json!(["aria-label","Inside shadow"]),vec![])]}]);
        let mut iframe = element(3, "IFRAME", json!([]), vec![]);
        iframe["contentDocument"] = json!({"nodeType":9,"nodeId":101,"backendNodeId":101,"childNodeCount":1,
            "children":[element(2,"BUTTON",json!(["aria-label","Inside frame"]),vec![])]});
        let (rows, coverage) = collect(&document(vec![host, iframe]), "opaque-main", 100, 20);
        let shadow = rows
            .iter()
            .find(|row| row.name.as_deref() == Some("Inside shadow"))
            .unwrap();
        let child = rows
            .iter()
            .find(|row| row.name.as_deref() == Some("Inside frame"))
            .unwrap();
        assert_eq!(shadow.frame_key, "opaque-main");
        assert_eq!(shadow.parent_backend_node_id, Some(1));
        assert_ne!(shadow.frame_key, child.frame_key);
        assert_eq!(child.parent_backend_node_id, None);
        let selected = project(
            &rows,
            &DomLocator::default(),
            Some(("opaque-main", 1)),
            None,
            20,
        )
        .unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|row| row.frame_key == "opaque-main"));
        assert!(coverage.complete);
    }

    #[test]
    fn missing_children_depth_and_node_budgets_do_not_claim_complete_absence() {
        let mut incomplete = element(1, "DIV", json!([]), vec![]);
        incomplete["childNodeCount"] = json!(3);
        let (rows, partial) = collect(&document(vec![incomplete]), "main", 100, 20);
        assert_eq!(rows.len(), 1);
        assert!(!partial.complete);
        assert_eq!(partial.node_omitted, 3);
        let deep = document(vec![element(
            1,
            "DIV",
            json!([]),
            vec![element(2, "SPAN", json!([]), vec![])],
        )]);
        let (_, depth) = collect(&deep, "main", 100, 1);
        assert!(!depth.complete);
        assert_eq!(depth.depth_omitted, 1);
        let (bounded, nodes) = collect(&deep, "main", 2, 20);
        assert_eq!(bounded.len(), 1);
        assert_eq!(nodes.visited, 2);
        assert!(!nodes.complete);
        assert_eq!(nodes.node_omitted, 1);
        for (max_nodes, max_depth) in [(0, 20), (2001, 20), (10, 0), (10, 65)] {
            let (rows, coverage) = collect(&deep, "main", max_nodes, max_depth);
            assert!(rows.is_empty());
            assert!(!coverage.complete);
            assert!(coverage.node_omitted > 0);
        }
    }

    #[test]
    fn exact_semantic_locators_keep_duplicate_labels_and_do_not_rank_partial_words() {
        let rows = records();
        let locator = DomLocator {
            role: Some("button".into()),
            name: Some("Save".into()),
            ..Default::default()
        };
        let matched = project(&rows, &locator, None, None, 20).unwrap();
        assert_eq!(
            matched
                .iter()
                .map(|row| row.backend_node_id)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(project(
            &rows,
            &DomLocator {
                role: Some("but".into()),
                ..Default::default()
            },
            None,
            None,
            20
        )
        .unwrap()
        .is_empty());
        assert_eq!(
            project(
                &rows,
                &DomLocator {
                    label: Some("Save".into()),
                    test_id: Some("save".into()),
                    ..Default::default()
                },
                None,
                None,
                20
            )
            .unwrap()
            .len(),
            1
        );
        assert_eq!(
            project(
                &rows,
                &DomLocator {
                    text: Some("draft".into()),
                    ..Default::default()
                },
                None,
                None,
                20
            )
            .unwrap()
            .len(),
            2
        );
    }

    #[test]
    fn css_requires_native_matches_and_never_accepts_a_foreign_frame_backend_id() {
        let rows = records();
        let locator = DomLocator {
            css: Some("button".into()),
            ..Default::default()
        };
        assert_eq!(
            project(&rows, &locator, None, None, 20).unwrap_err(),
            "dom_css_resolution_required"
        );
        let matches = HashSet::from([("foreign-frame".into(), 2)]);
        assert!(project(&rows, &locator, None, Some(&matches), 20)
            .unwrap()
            .is_empty());
        let matches = HashSet::from([("frame-main".into(), 2)]);
        assert_eq!(
            project(&rows, &locator, None, Some(&matches), 20)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn unknown_ambiguous_scopes_and_invalid_queries_refuse_exactly() {
        let mut rows = records();
        assert_eq!(
            project(
                &rows,
                &DomLocator::default(),
                Some(("foreign", 1)),
                None,
                20
            )
            .unwrap_err(),
            "dom_scope_not_found"
        );
        rows.push(rows[0].clone());
        assert_eq!(
            project(
                &rows,
                &DomLocator::default(),
                Some(("frame-main", 1)),
                None,
                20
            )
            .unwrap_err(),
            "dom_scope_ambiguous"
        );
        for locator in [
            DomLocator {
                text: Some("x".repeat(257)),
                ..Default::default()
            },
            DomLocator {
                css: Some("  ".into()),
                ..Default::default()
            },
        ] {
            assert_eq!(
                project(&[], &locator, None, None, 20).unwrap_err(),
                "dom_locator_invalid"
            );
        }
        assert_eq!(
            project(&[], &DomLocator::default(), None, None, 0).unwrap_err(),
            "dom_projection_limit_invalid"
        );
        assert!(serde_json::from_value::<DomLocator>(json!({"javascript":"anything"})).is_err());
    }

    #[test]
    fn strings_and_attribute_retention_are_bounded_without_splitting_unicode() {
        let mut attrs = vec![
            Value::String("aria-label".into()),
            Value::String("Ж".repeat(1100)),
        ];
        for i in 0..100 {
            attrs.extend([json!(format!("attribute-{i}")), json!("value")]);
        }
        let (rows, coverage) = collect(
            &document(vec![element(
                1,
                "DIV",
                json!(attrs),
                vec![text(10, &"🦀".repeat(1100))],
            )]),
            "main",
            100,
            20,
        );
        assert!(rows[0].text.chars().count() <= 1024);
        assert!(rows[0].name.as_ref().unwrap().chars().count() <= 1024);
        assert!(rows[0].attributes.len() <= 64);
        assert!(rows[0].text_truncated);
        assert_eq!(coverage.text_truncated, 1);
        assert!(!coverage.complete);
    }

    #[test]
    fn unsupported_shadow_or_unavailable_embedded_documents_are_truthfully_partial() {
        let mut host = element(1, "DIV", json!([]), vec![]);
        host["shadowRoots"] = json!([{"nodeType":11,"shadowRootType":"closed","childNodeCount":1,"children":[element(2,"BUTTON",json!([]),vec![])]}]);
        let frame = element(3, "IFRAME", json!([]), vec![]);
        let (rows, coverage) = collect(&document(vec![host, frame]), "main", 100, 20);
        assert_eq!(rows.len(), 2);
        assert!(!coverage.complete);
        assert!(coverage.node_omitted >= 2);
    }
}
