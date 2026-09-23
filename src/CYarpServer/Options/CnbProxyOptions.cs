namespace CYarpServer.Options
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
        /// Gets or sets the one shared API key of this server, required from public callers as
        /// <c>Authorization: Bearer &lt;ApiKey&gt;</c> on the chat endpoint.
        /// Leave empty to disable the check (then anyone who can reach the server may call chat).
        /// The CNB token never reaches this side; callers use this key instead.
        /// <para>
        /// The name is ApiKey, not ProxyKey: an old "ProxyKey" entry is not recognised and would
        /// silently leave the server unauthenticated, so it is worth double checking after an upgrade.
        /// </para>
        /// </summary>
        public string? ApiKey { get; set; }

        /// <summary>
        /// Gets or sets the model ids advertised on /v1/models, e.g.
        /// <c>"ProxyModels": [ "deepseek-v4-flash", "glm-5.3-flash", "kimi-k3" ]</c>.
        /// A comma separated string is not accepted: the configuration binder fails loudly
        /// instead of advertising an empty list.
        /// </summary>
        public string[]? ProxyModels { get; set; }
    }
}
