using Microsoft.AspNetCore.Http;
using Microsoft.Net.Http.Headers;
using System;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
using Yarp.ReverseProxy.Forwarder;

namespace CYarpServer.Forwarding
{
    /// <summary>
    /// 把公网请求改写成 CNB AI 请求：
    /// <list type="bullet">
    /// <item>把公网路径映射为 <c>/chat/completions</c>（客户端的目标 Uri 本身已经带有
    /// <c>/{org}/{repo}/-/ai</c>，而唯一被转发的路由是
    /// <c>/{org}/{repo}/v1/chat/completions</c>）</item>
    /// <item>把调用方的 Authorization（公网 ApiKey）替换成 CYarp 客户端在握手时报上来的 CNB token</item>
    /// <item>丢弃调用方的 Host，让上游边缘看到它实际服务的那个主机名</item>
    /// </list>
    /// </summary>
    internal sealed class CnbAiTransformer(string? upstreamAuthorization) : HttpTransformer
    {
        /// <summary>
        /// 本转换器请求的上游路径，相对于客户端的目标 Uri。
        /// </summary>
        private const string UpstreamPath = "/chat/completions";

        /// <summary>
        /// 改写请求：重写路径、替换凭据、清掉 Host。
        /// </summary>
        /// <param name="httpContext">当前请求上下文。</param>
        /// <param name="proxyRequest">即将发往上游的请求。</param>
        /// <param name="destinationPrefix">客户端目标 Uri 作为目的前缀。</param>
        /// <param name="cancellationToken">取消令牌。</param>
        /// <returns>表示异步操作的任务。</returns>
        public override async ValueTask TransformRequestAsync(
            HttpContext httpContext,
            HttpRequestMessage proxyRequest,
            string destinationPrefix,
            CancellationToken cancellationToken)
        {
            await base.TransformRequestAsync(httpContext, proxyRequest, destinationPrefix, cancellationToken);

            // 绝不让调用方自己的凭据（公网 ApiKey）到达上游：先丢掉，再注入
            // CYarp 客户端握手时报上来的 CNB token。
            proxyRequest.Headers.Remove(HeaderNames.Authorization);
            if (string.IsNullOrEmpty(upstreamAuthorization) == false)
            {
                proxyRequest.Headers.TryAddWithoutValidation(HeaderNames.Authorization, upstreamAuthorization);
            }

            proxyRequest.RequestUri = new Uri(destinationPrefix.TrimEnd('/') + UpstreamPath + httpContext.Request.QueryString.Value);

            // 绝不转发调用方的 Host（我们的公网域名 / IP）：上游边缘按 Host 路由，
            // CNB 边缘遇到不认识的主机会回 "301 Moved Permanently -> https://cnb.cool"，
            // 而不是正常提供 AI 端点。清空它之后，HttpClient 会从目标 Uri 推导出 Host。
            proxyRequest.Headers.Host = null;
        }
    }
}
