namespace CYarpServer
{
    /// <summary>
    /// Options of the public OpenAI-compatible surface published by this server.
    /// Bound from configuration (appsettings.json / environment variables) with the
    /// source generated configuration binder, so it stays Native AOT friendly.
    /// </summary>
    public sealed class CnbProxyOptions
    {
        /// <summary>
        /// Fallback model list advertised on /v1/models.
        /// CNB's AI API exposes no model-listing endpoint (its OpenAPI document only has
        /// /{repo}/-/ai/chat/completions), but OpenAI clients probe /v1/models on startup,
        /// so the server answers that request locally.
        /// </summary>
        public static readonly string[] DefaultModels = ["deepseek-v4-flash", "glm-5.3-flash", "kimi-k3"];

        /// <summary>
        /// Gets or sets the bearer token required from public callers.
        /// Leave empty to disable the inbound check.
        /// The CNB token never reaches this side; callers use their own key.
        /// </summary>
        public string? ProxyKey { get; set; }

        /// <summary>
        /// Gets or sets the model ids advertised on /v1/models, e.g.
        /// <c>"ProxyModels": [ "deepseek-v4-flash", "glm-5.3-flash", "kimi-k3" ]</c>.
        /// A comma separated string is not accepted: the configuration binder fails loudly
        /// instead of advertising an empty list.
        /// </summary>
        public string[]? ProxyModels { get; set; }
    }
}
