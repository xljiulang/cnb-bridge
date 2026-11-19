use super::*;

#[derive(Default, Clone)]
pub struct ToolShape {
    namespaces: HashMap<String, (String, String)>,
    pub(super) custom: HashSet<String>,
}

/// Tool declarations after deduplication by their flattened Chat name.
///
/// `translate_tools` (what the upstream is told) and `ToolShape` (how call
/// names are mapped back) both read this so they cannot disagree about which
/// declaration won a name collision.
#[derive(Default)]
pub(super) struct DeclaredTools {
    /// Deduplicated declarations in request order. Never reordered: the prompt
    /// prefix must stay byte-stable for upstream prefix caching.
    pub(super) tools: Vec<Value>,
    /// Name collisions and unrecognized carriers, surfaced by diagnostics.
    pub(super) notes: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolSource {
    TopLevel,
    AdditionalTools,
    ToolSearchOutput,
}

impl ToolSource {
    fn label(self) -> &'static str {
        match self {
            Self::TopLevel => "top",
            Self::AdditionalTools => "additional_tools",
            Self::ToolSearchOutput => "tool_search_output",
        }
    }
}

/// Collects every tool a request declares: the top-level `tools` array first,
/// then carriers embedded in `input`.
///
/// Top-level declarations win over carriers, and earlier carriers win over
/// later ones. Dropped declarations are recorded in `notes` instead of being
/// silently merged, because the response side must never resolve a call name
/// back to a declaration the upstream was not given.
pub(super) fn collect_declared_tools(request: &Value) -> DeclaredTools {
    let mut declared = DeclaredTools::default();
    // A compaction turn is a summarization request: exposing tools would let
    // the model answer with a tool call the client never asked for.
    if super::request::is_compaction_turn(request) {
        return declared;
    }
    let mut seen: HashMap<String, ToolSource> = HashMap::new();
    if let Some(tools) = request.get("tools").and_then(Value::as_array) {
        absorb_tools(ToolSource::TopLevel, tools, &mut seen, &mut declared);
    }
    if let Some(items) = request.get("input").and_then(Value::as_array) {
        for item in items {
            let source = match item.get("type").and_then(Value::as_str) {
                Some("additional_tools") => ToolSource::AdditionalTools,
                Some("tool_search_output") => ToolSource::ToolSearchOutput,
                _ => continue,
            };
            if let Some(tools) = item.get("tools").and_then(Value::as_array) {
                absorb_tools(source, tools, &mut seen, &mut declared);
            }
        }
    }
    declared
}

fn absorb_tools(
    source: ToolSource,
    tools: &[Value],
    seen: &mut HashMap<String, ToolSource>,
    declared: &mut DeclaredTools,
) {
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(children) = tool.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                let mut kept = Vec::new();
                for child in children {
                    let Some(name) = child.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    if admit(
                        &flat_namespace_name(namespace, name),
                        source,
                        seen,
                        declared,
                    ) {
                        kept.push(child.clone());
                    }
                }
                if kept.is_empty() {
                    continue;
                }
                let mut clone = tool.clone();
                clone["tools"] = Value::Array(kept);
                declared.tools.push(clone);
            }
            Some("tool_search") => {
                // Codex routes tool calls by item variant, so exposing a
                // `tool_search` function without restoring `tool_search_call`
                // would hand the client a name it cannot dispatch.
                declared
                    .notes
                    .push("tools.untracked:tool_search".to_string());
            }
            _ => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if admit(name, source, seen, declared) {
                    declared.tools.push(tool.clone());
                }
            }
        }
    }
}

fn admit(
    flat: &str,
    source: ToolSource,
    seen: &mut HashMap<String, ToolSource>,
    declared: &mut DeclaredTools,
) -> bool {
    if let Some(kept) = seen.get(flat) {
        declared.notes.push(format!(
            "tools.duplicate:{flat} kept={} dropped={}",
            kept.label(),
            source.label()
        ));
        return false;
    }
    seen.insert(flat.to_string(), source);
    true
}

impl ToolShape {
    /// Builds the request-local tool shape from Responses declarations.
    pub fn from_request(request: &Value) -> Self {
        Self::from_declared(&collect_declared_tools(request).tools)
    }

    /// Builds the reverse map from declarations the upstream was actually given.
    pub(super) fn from_declared(tools: &[Value]) -> Self {
        let mut shape = Self::default();
        for tool in tools {
            match tool.get("type").and_then(Value::as_str) {
                Some("namespace") => {
                    let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let Some(children) = tool.get("tools").and_then(Value::as_array) else {
                        continue;
                    };
                    for child in children {
                        let Some(name) = child.get("name").and_then(Value::as_str) else {
                            continue;
                        };
                        shape
                            .namespaces
                            .entry(flat_namespace_name(namespace, name))
                            .or_insert_with(|| (namespace.to_string(), name.to_string()));
                    }
                }
                Some("custom") => {
                    if let Some(name) = tool.get("name").and_then(Value::as_str) {
                        shape.custom.insert(name.to_string());
                    }
                }
                _ => {}
            }
        }
        shape
    }

    pub(super) fn resolve(&self, name: &str) -> (String, Option<String>) {
        match self.namespaces.get(name) {
            Some((namespace, tool)) => (tool.clone(), Some(namespace.clone())),
            None => (name.to_string(), None),
        }
    }

    pub(super) fn is_custom(&self, name: &str) -> bool {
        self.custom.contains(name)
    }
}

pub(super) fn translate_tools(tools: &[Value]) -> Vec<Value> {
    let mut translated = Vec::new();
    let mut seen = HashSet::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                let Some(children) = tool.get("tools").and_then(Value::as_array) else {
                    continue;
                };
                for child in children {
                    let Some(name) = child.get("name").and_then(Value::as_str) else {
                        continue;
                    };
                    let flat = flat_namespace_name(namespace, name);
                    if seen.insert(flat.clone()) {
                        translated.push(function_tool(&flat, child));
                    }
                }
            }
            Some("custom") => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if seen.insert(name.to_string()) {
                    let description = custom_tool_description(
                        tool.get("description")
                            .and_then(Value::as_str)
                            .unwrap_or(""),
                        name,
                    );
                    translated.push(json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": {
                                "type": "object",
                                "properties": {
                                    "input": {
                                        "type": "string",
                                        "description": "The complete free-form tool input."
                                    }
                                },
                                "required": ["input"]
                            }
                        }
                    }));
                }
            }
            _ => {
                let Some(name) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                if seen.insert(name.to_string()) {
                    translated.push(function_tool(name, tool));
                }
            }
        }
    }
    translated
}

pub(super) fn function_tool(name: &str, source: &Value) -> Value {
    let mut function = json!({ "name": name });
    if let Some(description) = source.get("description").filter(|value| !value.is_null()) {
        function["description"] = description.clone();
    }
    function["parameters"] = normalize_parameters(source.get("parameters"));
    json!({ "type": "function", "function": function })
}

/// Normalizes a function's `parameters` JSON Schema for strict Chat upstreams.
///
/// Responses tools may declare `parameters: null`, `{"type": null}` or
/// `required: null`, and some gateways reject `additionalProperties: false`
/// without an accompanying `required` array. Only schema-bearing keywords are
/// visited, so literal values under `default`/`examples` stay untouched.
pub(super) fn normalize_parameters(parameters: Option<&Value>) -> Value {
    let mut parameters = match parameters {
        Some(Value::Object(object)) => Value::Object(object.clone()),
        _ => return json!({ "type": "object", "properties": {} }),
    };
    sanitize_parameter_schema(&mut parameters, true);
    parameters
}

fn sanitize_parameter_schema(schema: &mut Value, is_root: bool) {
    let Value::Object(object) = schema else {
        return;
    };
    if is_root && object.get("type").and_then(Value::as_str) != Some("object") {
        object.insert("type".to_string(), json!("object"));
    }
    let invalid_required = object
        .get("required")
        .is_some_and(|required| !required.is_array());
    let strict_object_without_required = object.get("required").is_none()
        && object.get("additionalProperties") == Some(&Value::Bool(false));
    if invalid_required || strict_object_without_required {
        object.insert("required".to_string(), json!([]));
    }

    // Only keywords whose values are themselves schemas are recursed into.
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
        "dependencies",
    ] {
        if let Some(children) = object.get_mut(key).and_then(Value::as_object_mut) {
            for child in children.values_mut() {
                sanitize_schema_child(child);
            }
        }
    }
    for key in [
        "items",
        "additionalItems",
        "contains",
        "propertyNames",
        "additionalProperties",
        "unevaluatedItems",
        "unevaluatedProperties",
        "contentSchema",
        "not",
        "if",
        "then",
        "else",
    ] {
        if let Some(child) = object.get_mut(key) {
            sanitize_schema_child(child);
        }
    }
    for key in ["prefixItems", "allOf", "anyOf", "oneOf"] {
        if let Some(children) = object.get_mut(key).and_then(Value::as_array_mut) {
            for child in children {
                sanitize_schema_child(child);
            }
        }
    }
}

fn sanitize_schema_child(value: &mut Value) {
    match value {
        Value::Object(_) => sanitize_parameter_schema(value, false),
        Value::Array(children) => {
            for child in children {
                sanitize_schema_child(child);
            }
        }
        _ => {}
    }
}

pub(super) fn custom_tool_description(raw: &str, name: &str) -> String {
    let hint =
        "This is a function call. Put the complete free-form tool input in the JSON \"input\" field.";
    if raw.trim().is_empty() {
        format!("Run the {name} tool.\n\n{hint}")
    } else {
        format!("{}\n\n{hint}", raw.trim())
    }
}

pub(super) fn translate_tool_choice(choice: Option<&Value>) -> Option<Value> {
    let choice = choice.filter(|value| !value.is_null())?;
    match choice {
        Value::String(value) => Some(json!(value)),
        Value::Object(object) if object.get("type").and_then(Value::as_str) == Some("function") => {
            let name = object.get("name").and_then(Value::as_str)?;
            Some(json!({ "type": "function", "function": { "name": name } }))
        }
        _ => None,
    }
}

/// Flattened Chat names contributed by a list of Responses tool declarations.
pub(super) fn declared_tool_names(tools: &[Value]) -> Vec<String> {
    let mut names = Vec::new();
    for tool in tools {
        match tool.get("type").and_then(Value::as_str) {
            Some("namespace") => {
                let Some(namespace) = tool.get("name").and_then(Value::as_str) else {
                    continue;
                };
                for child in tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = child.get("name").and_then(Value::as_str) {
                        names.push(flat_namespace_name(namespace, name));
                    }
                }
            }
            _ => {
                if let Some(name) = tool.get("name").and_then(Value::as_str) {
                    names.push(name.to_string());
                }
            }
        }
    }
    names
}

pub(super) fn flat_namespace_name(namespace: &str, name: &str) -> String {
    format!("{namespace}_{name}")
}

pub(super) fn normalize_arguments(raw: &str) -> (String, bool) {
    let raw = raw.trim();
    if raw.is_empty() {
        return ("{}".to_string(), false);
    }
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(_)) => (raw.to_string(), false),
        _ => ("{}".to_string(), true),
    }
}
