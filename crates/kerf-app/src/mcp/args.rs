//! Strict tool arguments. serde ignores a key a parameter struct does not name, so
//! `trim { rippel: false }` would trim with the project's ripple mode and report
//! success — the model never learns its override was dropped. A key the tool's
//! input schema does not list is refused instead, naming the ones it does.

use std::collections::BTreeSet;

use rmcp::model::{JsonObject, Tool};
use serde_json::Value;

/// Why `args` do not fit `tool`, if they do not: the first unknown key and the accepted ones.
pub(super) fn unknown_argument(tool: &Tool, args: Option<&JsonObject>) -> Option<String> {
    let accepted = accepted_keys(&tool.input_schema)?;
    let bad = args?.keys().find(|k| !accepted.contains(k.as_str()))?;
    let expected = if accepted.is_empty() {
        "it takes no arguments".to_string()
    } else {
        format!("expected: {}", accepted.into_iter().collect::<Vec<_>>().join(", "))
    };
    Some(format!("unknown argument \"{bad}\" for {}; {expected}", tool.name))
}

/// Every top-level key `schema` admits, or `None` when it admits any key (a
/// flattened map, `additionalProperties`). A flattened struct or enum shows up as
/// `allOf` / `anyOf` / `oneOf` branches, possibly behind a `$ref`, so those are
/// followed and their properties merged.
fn accepted_keys(schema: &JsonObject) -> Option<BTreeSet<&str>> {
    let mut keys = BTreeSet::new();
    collect(schema, schema, &mut keys, 0).then_some(keys)
}

/// Adds `node`'s keys to `keys`; false when `node` admits any key.
fn collect<'a>(root: &'a JsonObject, node: &'a JsonObject, keys: &mut BTreeSet<&'a str>, depth: usize) -> bool {
    if depth > 16 {
        return false;
    }
    if node.get("additionalProperties").is_some_and(|a| a != &Value::Bool(false)) {
        return false;
    }
    if let Some(props) = node.get("properties").and_then(Value::as_object) {
        keys.extend(props.keys().map(String::as_str));
    }
    if let Some(target) = node.get("$ref").and_then(Value::as_str) {
        match resolve(root, target) {
            Some(target) if collect(root, target, keys, depth + 1) => {}
            _ => return false,
        }
    }
    for branch in ["allOf", "anyOf", "oneOf"] {
        for sub in node.get(branch).and_then(Value::as_array).into_iter().flatten() {
            match sub.as_object() {
                Some(sub) if collect(root, sub, keys, depth + 1) => {}
                _ => return false,
            }
        }
    }
    true
}

/// A local `#/$defs/Name` (or `#/definitions/Name`) reference.
fn resolve<'a>(root: &'a JsonObject, target: &str) -> Option<&'a JsonObject> {
    let mut node = root;
    for part in target.strip_prefix("#/")?.split('/') {
        node = node.get(part)?.as_object()?;
    }
    Some(node)
}

#[cfg(test)]
mod tests {
    use super::unknown_argument;
    use rmcp::model::{JsonObject, Tool};
    use rmcp::schemars;
    use serde_json::json;

    fn tool<T: schemars::JsonSchema + 'static>() -> Tool {
        let schema = rmcp::handler::server::common::schema_for_input::<T>().unwrap();
        Tool::new("t", "test", schema)
    }

    fn args(v: serde_json::Value) -> JsonObject {
        v.as_object().unwrap().clone()
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Inner {
        at: f64,
        ripple: Option<bool>,
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Outer {
        clip_id: String,
        #[serde(flatten)]
        inner: Inner,
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[serde(tag = "kind", rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Shape {
        Rect { w: f64 },
        Ellipse { rx: f64 },
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct WithEnum {
        clip_id: String,
        #[serde(flatten)]
        shape: Shape,
    }

    #[derive(serde::Deserialize, schemars::JsonSchema)]
    #[allow(dead_code)]
    struct Open {
        clip_id: String,
        #[serde(flatten)]
        rest: std::collections::HashMap<String, serde_json::Value>,
    }

    #[test]
    fn a_flattened_struct_contributes_its_fields() {
        let t = tool::<Outer>();
        assert_eq!(
            unknown_argument(&t, Some(&args(json!({"clip_id": "a", "at": 1.0, "ripple": false})))),
            None
        );
        let said = unknown_argument(&t, Some(&args(json!({"clip_id": "a", "rippel": false})))).unwrap();
        assert!(said.contains("\"rippel\"") && said.contains("at, clip_id, ripple"), "{said}");
    }

    #[test]
    fn a_flattened_enum_contributes_every_variant() {
        let t = tool::<WithEnum>();
        for ok in [
            json!({"clip_id": "a", "kind": "rect", "w": 1.0}),
            json!({"clip_id": "a", "kind": "ellipse", "rx": 1.0}),
        ] {
            assert_eq!(unknown_argument(&t, Some(&args(ok))), None);
        }
        assert!(unknown_argument(&t, Some(&args(json!({"clip_id": "a", "h": 1.0})))).is_some());
    }

    #[test]
    fn a_flattened_map_admits_anything() {
        let t = tool::<Open>();
        assert_eq!(
            unknown_argument(&t, Some(&args(json!({"clip_id": "a", "whatever": 1})))),
            None
        );
    }

    #[test]
    fn a_tool_without_arguments_says_so() {
        let t = Tool::new("list_assets", "test", rmcp::handler::server::common::schema_for_empty_input());
        assert_eq!(unknown_argument(&t, None), None);
        assert_eq!(unknown_argument(&t, Some(&JsonObject::new())), None);
        let said = unknown_argument(&t, Some(&args(json!({"verbose": true})))).unwrap();
        assert_eq!(said, "unknown argument \"verbose\" for list_assets; it takes no arguments");
    }
}
