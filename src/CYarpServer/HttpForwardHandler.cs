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
    /// <item><c>/{clientId}/v1/chat/completions</c> - addresses one workspace explicitly</item>
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

            // 2. Path prefix: /{clientId}/v1/...
            var (first, rest) = SplitFirstSegment(rawPath);
            if (first.Length > 0 && string.Equals(first, "v1", StringComparison.OrdinalIgnoreCase) == false)
            {
                return new RouteTarget(
                    client: viewer.TryGetValue(first, out var byPath) ? byPath : null,
                    prefix: first,
                    path: rest);
            }

            // 3. Bare OpenAI path: served while exactly one workspace is connected.
            return new RouteTarget(
                client: viewer.Count == 1 ? viewer.First() : null,
                prefix: null,
                path: rawPath);
        }

        /// <summary>
        /// Splits "/org~repo/v1/models" into "org~repo" and "/v1/models".
        /// </summary>
        private static (string First, string Remaining) SplitFirstSegment(string path)
        {
            var next = path.IndexOf('/', 1);
            return next < 0
                ? (path.TrimStart('/'), "/")
                : (path[1..next], path[next..]);
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
