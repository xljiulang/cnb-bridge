namespace CYarpServer.Options
{
    /// <summary>
    /// 本服务端对外发布的 OpenAI 兼容接口的配置。
    /// <para>
    /// 从配置（appsettings.json / 环境变量）用源生成绑定器绑定，因此对 Native AOT 友好。
    /// </para>
    /// </summary>
    public sealed class CnbProxyOptions
    {
        /// <summary>
        /// /v1/models 上公布的兜底模型列表。
        /// <para>
        /// CNB 的 AI API 没有列模型的端点（其 OpenAPI 文档里只有 /{repo}/-/ai/chat/completions），
        /// 但 OpenAI 客户端启动时会探 /v1/models，所以这个请求由服务端本地应答。
        /// </para>
        /// </summary>
        public static readonly string[] DefaultModels = ["deepseek-v4-flash", "glm-5.3-flash", "kimi-k3"];

        /// <summary>
        /// 获取或设置本服务端唯一的共享 API key：公网调用方在 chat 端点上要带
        /// <c>Authorization: Bearer &lt;ApiKey&gt;</c>。留空则不校验（此时任何能访问到服务端
        /// 的人都能调用 chat）。CNB token 从不到达这一侧，调用方用的是这个 key。
        /// <para>
        /// 键名是 ApiKey，不是 ProxyKey：写成旧名 "ProxyKey" 不会被识别，服务端会静默变成不校验，
        /// 所以升级后值得再核一遍。
        /// </para>
        /// </summary>
        public string? ApiKey { get; set; }

        /// <summary>
        /// 获取或设置在 /v1/models 上公布的模型 id，例如
        /// <c>"ProxyModels": [ "deepseek-v4-flash", "glm-5.3-flash", "kimi-k3" ]</c>。
        /// 不接受逗号分隔的字符串：配置绑定会直接报错，而不是公布一个空列表。
        /// </summary>
        public string[]? ProxyModels { get; set; }
    }
}
