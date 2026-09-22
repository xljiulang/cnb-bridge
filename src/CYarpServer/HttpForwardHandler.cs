using CYarp.Server;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Diagnostics;
using System.Linq;
using System.Security.Claims;
using System.Threading.Tasks;

namespace CYarpServer
{
    /// <summary>
    /// Forwards public OpenAI-compatible requests through the CYarp tunnel of the
    /// workspace addressed by the request, and answers /v1/models locally.
    /// <para>
    /// Public request shapes:
    /// <list type="bullet">
    /// <item><c>/{clientId}/v1/chat/completions</c> - addresses one workspace explicitly;
    /// the id is the CNB repo slug, so the common shape is <c>/{org}/{repo}/v1/...</c></item>
    /// <item><c>/v1/chat/completions</c> - works while exactly one workspace is connected</item>
    /// <item>header <c>X-CYarp-Client: {clientId}</c> - overrides the path prefix</item>
    /// </list>
    /// </para>
    /// </summary>
    public static partial class HttpForwardHandler
    {
        private const string ModelsPath = "/v1/models";

        /// <summary>
        /// Handle CYarp forwarding.
        /// </summary>
        public static async Task HandleAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var options = services.GetRequiredService<IOptions<CnbProxyOptions>>().Value;
            var logger = services.GetRequiredService<ILoggerFactory>().CreateLogger(LogCategory);

            var target = Resolve(context, viewer);

            if (IsModelsRequest(context.Request.Method, target.Path))
            {
                // A workspace prefix that is not connected means "server is up but the
                // tunnel is dead". /v1/models is answered locally, so this probe costs
                // no AI quota, yet it still detects a fake-alive workspace.
                if (target.Prefix != null && target.Client == null)
                {
                    LogWorkspaceOffline(logger, target.Prefix, context.Request.Path.Value ?? "/");
                    context.Response.StatusCode = StatusCodes.Status503ServiceUnavailable;
                    return;
                }

                await WriteModelsAsync(context, options);
                return;
            }

            if (target.Client == null)
            {
                LogUnroutable(logger, context.Request.Path.Value ?? "/", viewer.Count);
                context.Response.StatusCode = StatusCodes.Status502BadGateway;
                return;
            }

            // Strip the workspace prefix in place: the transformer below only has to
            // deal with the OpenAI "/v1" prefix.
            context.Request.Path = target.Path;

            // The CNB token travels in the handshake principal that CYarp keeps on IClient.User.
            var upstreamAuthorization = target.Client.User.FindFirstValue(CnbClaims.UpstreamAuthorization);

            var stopwatch = Stopwatch.StartNew();
            var error = await target.Client.ForwardHttpAsync(context, new CnbAiTransformer(upstreamAuthorization));
            stopwatch.Stop();

            if (error == Yarp.ReverseProxy.Forwarder.ForwarderError.None)
            {
                LogResponse(logger, target.Client.Id, context.Request.Method, context.Response.StatusCode, stopwatch.Elapsed);
            }
            else
            {
                LogForwardError(logger, target.Client.Id, context.Request.Method, error);
            }
        }

        private static Task WriteModelsAsync(HttpContext context, CnbProxyOptions options)
        {
            var models = (string.IsNullOrWhiteSpace(options.ProxyModels) ? CnbProxyOptions.DefaultModels : options.ProxyModels)
                .Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);

            var payload = new ModelListResponse
            {
                Data = models.Select(model => new ModelItem { Id = model, OwnedBy = "cnb" }).ToArray(),
            };

            return context.Response.WriteAsJsonAsync(payload, CnbJsonContext.Default.ModelListResponse);
        }

        private static bool IsModelsRequest(string method, string path)
        {
            if (HttpMethods.IsGet(method) == false)
            {
                return false;
            }

            // Tolerate the trailing slash some clients emit ("/v1/models/").
            return path.TrimEnd('/').Equals(ModelsPath, StringComparison.OrdinalIgnoreCase);
        }

        /// <summary>
        /// Resolves which client serves the request and which path has to be sent to it.
        /// The workspace prefix may contain slashes (the CNB repo slug is "org/repo"), so the
        /// prefix is matched against the connected workspaces, longest id first.
        /// </summary>
        private static RouteTarget Resolve(HttpContext context, IClientViewer viewer)
        {
            var rawPath = context.Request.Path.Value ?? "/";

            // 1. Explicit client id header (same header name the CYarp handshake uses).
            var headerClientId = context.Request.Headers[CYarpServerHost.ClientIdHeader].ToString();
            if (string.IsNullOrEmpty(headerClientId) == false)
            {
                return new RouteTarget(
                    client: viewer.TryGetValue(headerClientId, out var byHeader) ? byHeader : null,
                    prefix: headerClientId,
                    path: rawPath);
            }

            // 2. Path prefix: /{clientId}/v1/... (clientId may be "org/repo" or "org/repo/sub").
            var matchedId = FindLongestConnectedPrefix(rawPath, viewer);
            if (matchedId != null)
            {
                var remaining = rawPath.Length == matchedId.Length + 1 ? "/" : rawPath[(matchedId.Length + 1)..];
                return new RouteTarget(
                    client: viewer.TryGetValue(matchedId, out var byPath) ? byPath : null,
                    prefix: matchedId,
                    path: remaining);
            }

            // 3. Bare OpenAI path: served while exactly one workspace is connected.
            if (IsOpenAiPath(rawPath))
            {
                return new RouteTarget(
                    client: viewer.Count == 1 ? viewer.First() : null,
                    prefix: null,
                    path: rawPath);
            }

            // 4. No connected workspace matches: treat everything before the OpenAI "/v1"
            //    marker as an (offline) workspace prefix, so that /{org}/{repo}/v1/models
            //    still reports 503 (server up, tunnel down) instead of a generic 502.
            var (prefix, path) = SplitOfflinePrefix(rawPath);
            return new RouteTarget(client: null, prefix: prefix, path: path);
        }

        /// <summary>
        /// Finds the longest connected client id that is a path prefix of the request, so
        /// that "/org/repo/v1/models" resolves to the workspace "org/repo" (and
        /// "/org/repo/sub/v1/models" to "org/repo/sub" when both are connected).
        /// Comparison is ordinal (case sensitive), matching CYarp's own client id lookup.
        /// </summary>
        private static string? FindLongestConnectedPrefix(string path, IClientViewer viewer)
        {
            if (path.Length < 2 || path[0] != '/')
            {
                return null;
            }

            string? matched = null;
            var matchedLength = 0;

            foreach (var candidate in viewer)
            {
                var id = candidate.Id;
                if (id.Length == 0 || id.Length <= matchedLength)
                {
                    continue;
                }

                // id occupies path[1 .. id.Length]; the path must be exactly "/{id}" or continue
                // with another segment, i.e. "/{id}/...".
                if (path.Length <= id.Length || string.CompareOrdinal(path, 1, id, 0, id.Length) != 0)
                {
                    continue;
                }

                if (path.Length > id.Length + 1 && path[id.Length + 1] != '/')
                {
                    continue;
                }

                matched = id;
                matchedLength = id.Length;
            }

            return matched;
        }

        private static bool IsOpenAiPath(string path)
        {
            return path.Equals("/v1", StringComparison.OrdinalIgnoreCase)
                || path.StartsWith("/v1/", StringComparison.OrdinalIgnoreCase);
        }

        /// <summary>
        /// Splits an unmatched path like "/org/repo/v1/models" into the (offline) workspace
        /// prefix "org/repo" and the API path "/v1/models". A path without the "/v1" marker
        /// keeps its whole leading part as the prefix.
        /// </summary>
        private static (string Prefix, string Path) SplitOfflinePrefix(string path)
        {
            var marker = path.LastIndexOf("/v1/", StringComparison.OrdinalIgnoreCase);
            if (marker > 0)
            {
                return (path[1..marker], path[marker..]);
            }

            var trimmed = path.TrimEnd('/');
            if (trimmed.EndsWith("/v1", StringComparison.OrdinalIgnoreCase))
            {
                var end = trimmed.Length - 3;
                if (end > 0)
                {
                    return (path[1..end], "/v1");
                }
            }

            return (path.TrimStart('/'), path);
        }

        private const string LogCategory = "CYarpServer.HttpForward";

        private readonly struct RouteTarget(IClient? client, string? prefix, string path)
        {
            public IClient? Client { get; } = client;

            /// <summary>
            /// Gets the workspace id the caller asked for, if any.
            /// </summary>
            public string? Prefix { get; } = prefix;

            /// <summary>
            /// Gets the path to forward, with the workspace prefix already stripped.
            /// </summary>
            public string Path { get; } = path;
        }

        [LoggerMessage(Level = LogLevel.Warning, Message = "Workspace '{clientId}' is not connected (tunnel down), request {path} rejected")]
        private static partial void LogWorkspaceOffline(ILogger logger, string clientId, string path);

        [LoggerMessage(Level = LogLevel.Warning, Message = "No workspace is addressed by {path} ({count} workspace(s) connected)")]
        private static partial void LogUnroutable(ILogger logger, string path, int count);

        [LoggerMessage(Level = LogLevel.Information, Message = "<= [{clientId}] {method} {statusCode}, elapsed {elapsed}")]
        private static partial void LogResponse(ILogger logger, string clientId, string method, int statusCode, TimeSpan elapsed);

        [LoggerMessage(Level = LogLevel.Error, Message = "=> [{clientId}] {method} forwarding failed with {error}")]
        private static partial void LogForwardError(ILogger logger, string clientId, string method, Yarp.ReverseProxy.Forwarder.ForwarderError error);
    }
}
