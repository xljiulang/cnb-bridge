use super::*;

pub(super) struct ModelResponses {
    pub(super) list: Bytes,
    pub(super) details: BTreeMap<String, Bytes>,
}

impl ModelResponses {
    pub(super) fn from_catalog(catalog: &Value) -> Self {
        let models = catalog["models"]
            .as_array()
            .expect("validated model catalog has a models array");
        let list =
            serde_json::to_vec(&json!({ "object": "list", "data": models })).unwrap_or_default();
        let details = models
            .iter()
            .filter_map(|model| {
                let id = model["id"].as_str()?.to_string();
                let body = serde_json::to_vec(model).ok()?;
                Some((id, Bytes::from(body)))
            })
            .collect();
        Self {
            list: Bytes::from(list),
            details,
        }
    }
}

pub(super) async fn handle_models(State(state): State<GatewayState>) -> Response<Body> {
    json_bytes_response(state.models.list.clone())
}

pub(super) async fn handle_model(
    State(state): State<GatewayState>,
    Path(id): Path<String>,
) -> Response<Body> {
    match state.models.details.get(&id) {
        Some(model) => json_bytes_response(model.clone()),
        None => error_response(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("The model '{id}' does not exist"),
        ),
    }
}

pub(super) fn json_bytes_response(body: Bytes) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}
