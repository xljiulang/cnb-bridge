use super::*;

#[derive(Default, Clone)]
pub struct ToolShape {
    namespaces: HashMap<String, (String, String)>,
    pub(super) custom: HashSet<String>,
}

impl ToolShape {
    /// Builds the request-local tool shape from Responses declarations.
    pub fn from_request(request: &Value) -> Self {
        let mut shape = Self::default();
        let Some(tools) = request.get("tools").and_then(Value::as_array) else {
            return shape;
        };
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
                        shape.namespaces.insert(
                            flat_namespace_name(namespace, name),
                            (namespace.to_string(), name.to_string()),
                        );
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
    if let Some(description) = source.get("description") {
        function["description"] = description.clone();
    }
    if let Some(parameters) = source.get("parameters") {
        function["parameters"] = parameters.clone();
    }
    json!({ "type": "function", "function": function })
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
