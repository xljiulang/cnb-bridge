using Microsoft.AspNetCore.Http;
using Microsoft.Net.Http.Headers;
using System;
using System.Net.Http;
using System.Threading;
using System.Threading.Tasks;
using Yarp.ReverseProxy.Forwarder;

namespace CYarpServer
{
    /// <summary>
    /// Rewrites the public OpenAI-compatible request into the CNB AI request:
    /// <list type="bullet">
    /// <item>strips the leading "/v1" segment (the client's target Uri already carries
    /// "/{org}/{repo}/-/ai", so "/v1/chat/completions" must become "/chat/completions"
    /// relative to it)</item>
    /// <item>replaces the caller's Authorization (the public ProxyKey) with the CNB token
    /// that the CYarp client reported during its handshake</item>
    /// </list>
    /// The workspace path prefix (if any) has already been stripped from
    /// <see cref="HttpContext.Request"/> by the caller.
    /// </summary>
    internal sealed class CnbAiTransformer(string? upstreamAuthorization) : HttpTransformer
    {
        public override async ValueTask TransformRequestAsync(
            HttpContext httpContext,
            HttpRequestMessage proxyRequest,
            string destinationPrefix,
            CancellationToken cancellationToken)
        {
            await base.TransformRequestAsync(httpContext, proxyRequest, destinationPrefix, cancellationToken);

            // Never let the caller's own credential (the public ProxyKey) reach the upstream:
            // drop it, then inject the CNB token reported by the CYarp client handshake.
            proxyRequest.Headers.Remove(HeaderNames.Authorization);
            if (string.IsNullOrEmpty(upstreamAuthorization) == false)
            {
                proxyRequest.Headers.TryAddWithoutValidation(HeaderNames.Authorization, upstreamAuthorization);
            }

            var path = httpContext.Request.Path.Value ?? string.Empty;
            if (path.StartsWith("/v1/", StringComparison.OrdinalIgnoreCase))
            {
                path = path[3..];
            }
            else if (path.Equals("/v1", StringComparison.OrdinalIgnoreCase))
            {
                path = "/";
            }

            proxyRequest.RequestUri = new Uri(destinationPrefix.TrimEnd('/') + path + httpContext.Request.QueryString.Value);

            // Never forward the caller's Host (our public domain / IP): upstream edges route by
            // Host, and CNB's edge answers the wrong host with
            // "301 Moved Permanently -> https://cnb.cool" instead of serving the AI endpoint.
            // Clearing it makes HttpClient derive Host from the target Uri.
            proxyRequest.Headers.Host = null;
        }
    }
}
