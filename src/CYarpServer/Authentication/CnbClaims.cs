namespace CYarpServer.Authentication
{
    /// <summary>
    /// CYarp 隧道握手身份上携带的 claims。
    /// <para>
    /// CYarp 会把握手请求的 <see cref="System.Security.Claims.ClaimsPrincipal"/> 存到
    /// <see cref="CYarp.Server.IClient.User"/>，所以这里就是把工作空间身份与其上游凭据
    /// 送到转发代码的那条通道。
    /// </para>
    /// </summary>
    public static class CnbClaims
    {
        /// <summary>
        /// 工作空间 id，即 CNB 仓库 slug。
        /// </summary>
        public const string ClientId = "cnb:client-id";

        /// <summary>
        /// 客户端握手时报上来的 Authorization 头，形如 "Bearer &lt;cnb-token&gt;"。
        /// 服务端转发时原样带上，这样 CNB 才能认证这次调用。
        /// </summary>
        public const string UpstreamAuthorization = "cnb:upstream-authorization";

        /// <summary>
        /// 客户端隧道所指向的上游基址（仅用于诊断，不参与转发）。
        /// </summary>
        public const string TargetUri = "cnb:target-uri";
    }
}
