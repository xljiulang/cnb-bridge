use cnb_bridge::model_catalog;

#[test]
fn exposes_embedded_model_catalog_for_desktop_clients() {
    let models = model_catalog::summaries().expect("embedded model catalog should be valid");

    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        vec!["deepseek-v4.1-flash"]
    );

    let model = &models[0];
    assert_eq!(model.display_name, "DeepSeek-V4.1-Flash");
    assert_eq!(model.context_window, 1_048_576);
    assert_eq!(model.input_modalities, vec!["text", "image"]);
    assert!(model.supports_tools);
    assert!(model.supports_streaming);
    assert!(model.supports_reasoning_summaries);
}

#[test]
fn serializes_model_summaries_with_camel_case_fields() {
    let model = model_catalog::summaries()
        .expect("embedded model catalog should be valid")
        .remove(0);
    let value = serde_json::to_value(model).expect("model summary should serialize");

    assert_eq!(value["displayName"], "DeepSeek-V4.1-Flash");
    assert_eq!(value["contextWindow"], 1_048_576);
    assert_eq!(
        value["inputModalities"],
        serde_json::json!(["text", "image"])
    );
    assert_eq!(value["supportsTools"], true);
}
