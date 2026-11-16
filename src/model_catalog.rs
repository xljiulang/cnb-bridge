//! Compile-time model catalog shared by the HTTP gateway and desktop shell.

use serde::Serialize;
use serde_json::Value;

const MODELS_RESOURCE: &str = include_str!("../resources/models.json");

/// Model metadata needed by desktop client configuration views.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub id: String,
    pub display_name: String,
    pub description: Option<String>,
    pub context_window: u64,
    pub input_modalities: Vec<String>,
    pub supports_tools: bool,
    pub supports_streaming: bool,
    pub supports_reasoning_summaries: bool,
}

/// Parses and validates the embedded OpenAI-compatible model catalog.
pub fn catalog() -> Result<Value, String> {
    let value: Value = serde_json::from_str(MODELS_RESOURCE)
        .map_err(|error| format!("解析内置模型目录失败: {error}"))?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "内置模型目录缺少 models 数组".to_string())?;
    if models.is_empty()
        || models
            .iter()
            .any(|model| model.get("id").and_then(Value::as_str).is_none())
    {
        return Err("内置模型目录包含无效模型".to_string());
    }
    Ok(value)
}

/// Returns the model fields consumed by the desktop client.
pub fn summaries() -> Result<Vec<ModelSummary>, String> {
    let catalog = catalog()?;
    let models = catalog["models"]
        .as_array()
        .expect("catalog validation guarantees a models array");

    Ok(models
        .iter()
        .map(|model| {
            let id = model["id"]
                .as_str()
                .expect("catalog validation guarantees model IDs")
                .to_string();
            ModelSummary {
                display_name: model["display_name"].as_str().unwrap_or(&id).to_string(),
                description: model["description"].as_str().map(str::to_string),
                context_window: model["context_window"].as_u64().unwrap_or_default(),
                input_modalities: model["input_modalities"]
                    .as_array()
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                supports_tools: model["supports_tools"].as_bool().unwrap_or(false),
                supports_streaming: model["supports_streaming"].as_bool().unwrap_or(false),
                supports_reasoning_summaries: model["supports_reasoning_summaries"]
                    .as_bool()
                    .unwrap_or(false),
                id,
            }
        })
        .collect())
}
