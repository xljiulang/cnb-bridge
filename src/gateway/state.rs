use super::*;

#[derive(Clone)]
pub(super) struct GatewayState {
    pub(super) bridge: Arc<Bridge>,
    pub(super) activity: Arc<GatewayActivity>,
    pub(super) models: Arc<ModelResponses>,
    pub(super) activation_timeout: Duration,
    pub(super) non_stream_timeout: Duration,
    pub(super) client: reqwest::Client,
    pub(super) upstream_override: Option<u16>,
    pub(super) trace_store: HttpTraceStore,
    pub(super) logger: Arc<Logger>,
}
