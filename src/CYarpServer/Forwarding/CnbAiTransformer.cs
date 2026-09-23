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
    /// Rewrites the public request into the CNB AI request:
    /// <list type="bullet">
    /// <item>maps the public path to <c>/chat/completions</c> (the client's target Uri already
    /// carries <c>/{org}/{repo}/-/ai</c>, and the only forwarded route is
    /// <c>/{org}/{repo}/v1/chat/completions</c>)</item>
    /// <item>replaces the caller's Authorization (the public ApiKey) with the CNB token that
    /// the CYarp client reported during its handshake</item>
    /// <item>drops the caller's Host so the upstream edge sees the host it actually serves</item>
    /// </list>
    /// </summary>
    internal sealed class CnbAiTransformer(string? upstreamAuthorization) : HttpTransformer
    {
        /// <summary>
        /// Upstream path served by this transformer, relative to the client's target Uri.
        /// </summary>
        private const string UpstreamPath = "/chat/completions";

        public override async ValueTask TransformRequestAsync(
            HttpContext httpContext,
            HttpRequestMessage proxyRequest,
            string destinationPrefix,
            CancellationToken cancellationToken)
        {
            await base.TransformRequestAsync(httpContext, proxyRequest, destinationPrefix, cancellationToken);

            // Never let the caller's own credential (the public ApiKey) reach the upstream:
            // drop it, then inject the CNB token reported by the CYarp client handshake.
            proxyRequest.Headers.Remove(HeaderNames.Authorization);
            if (string.IsNullOrEmpty(upstreamAuthorization) == false)
            {
                proxyRequest.Headers.TryAddWithoutValidation(HeaderNames.Authorization, upstreamAuthorization);
            }

            proxyRequest.RequestUri = new Uri(destinationPrefix.TrimEnd('/') + UpstreamPath + httpContext.Request.QueryString.Value);

            // Never forward the caller's Host (our public domain / IP): upstream edges route by
            // Host, and CNB's edge answers the wrong host with
            // "301 Moved Permanently -> https://cnb.cool" instead of serving the AI endpoint.
            // Clearing it makes HttpClient derive Host from the target Uri.
            proxyRequest.Headers.Host = null;
        }
    }
}
