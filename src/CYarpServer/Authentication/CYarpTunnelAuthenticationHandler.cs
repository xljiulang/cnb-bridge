using Microsoft.AspNetCore.Authentication;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System.Collections.Generic;
using System.Security.Claims;
using System.Text.Encodings.Web;
using System.Threading.Tasks;

namespace CYarpServer.Authentication
{
    /// <summary>
    /// CYarp 客户端握手的认证方案。
    /// <para>
    /// CYarp 客户端把自己的身份与上游凭据作为握手头（ConnectHeaders）报上来。本处理器把它们
    /// 转成 claims，再由 CYarp 自己把由此得到的 <see cref="ClaimsPrincipal"/> 存到
    /// <see cref="CYarp.Server.IClient.User"/> —— 这就是上游凭据无需扩展 CYarp API
    /// 就能送到的原因。
    /// </para>
    /// <para>
    /// 刻意宽松：握手从不被拒绝。任何能访问到本服务端的客户端都可以注册隧道（也就可以冒用
    /// 任意工作空间 id），这是本次部署接受的取舍；在把服务端暴露给更大范围之前，
    /// 请先读 README 的安全说明。
    /// </para>
    /// </summary>
    internal sealed class CYarpTunnelAuthenticationHandler(
        IOptionsMonitor<AuthenticationSchemeOptions> options,
        ILoggerFactory logger,
        UrlEncoder encoder) : AuthenticationHandler<AuthenticationSchemeOptions>(options, logger, encoder)
    {
        public const string SchemeName = "CYarpTunnel";

        /// <summary>
        /// CYarp 客户端在握手时携带的、表示隧道目标 Uri 的头。只有 CYarp 握手会带它，
        /// 因此可以用它把隧道握手与公网 API 调用区分开。
        /// </summary>
        private const string TargetUriHeader = "CYarp-TargetUri";

        /// <summary>
        /// 处理握手请求的认证：把 CYarp 上报的握手头转成 claims。
        /// </summary>
        /// <returns>认证结果；公网 API 调用返回 NoResult（交给后面的 ApiKey 中间件）。</returns>
        protected override Task<AuthenticateResult> HandleAuthenticateAsync()
        {
            var targetUri = Request.Headers[TargetUriHeader].ToString();
            if (string.IsNullOrEmpty(targetUri))
            {
                // 公网 API 调用：保持匿名，由 ApiKey 中间件处理。
                return Task.FromResult(AuthenticateResult.NoResult());
            }

            var clientId = Request.Headers[CYarpServerHost.ClientIdHeader].ToString();
            if (string.IsNullOrEmpty(clientId))
            {
                clientId = CYarpServerHost.DefaultClientId;
            }

            var claims = new List<Claim>(3)
            {
                new(CnbClaims.ClientId, clientId),
                new(CnbClaims.TargetUri, targetUri),
            };

            var upstreamAuthorization = Request.Headers.Authorization.ToString();
            if (string.IsNullOrEmpty(upstreamAuthorization) == false)
            {
                claims.Add(new Claim(CnbClaims.UpstreamAuthorization, upstreamAuthorization));
            }

            var identity = new ClaimsIdentity(claims, SchemeName);
            var ticket = new AuthenticationTicket(new ClaimsPrincipal(identity), SchemeName);
            return Task.FromResult(AuthenticateResult.Success(ticket));
        }
    }
}
