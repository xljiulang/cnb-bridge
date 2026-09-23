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
    /// Handlers of the public OpenAI-compatible surface. The routes live in
    /// <see cref="CYarpServerHost"/> and come in two flavours:
    /// <list type="bullet">
    /// <item>workspace-addressed: <c>/{org}/{repo}/v1/models</c> and
    /// <c>/{org}/{repo}/v1/chat/completions</c> - the workspace id is the CNB repo slug, built
    /// from the two route values, so no path parsing happens here;</item>
    /// <item>bare (unified base_url): <c>GET /</c> lists the connected workspaces,
    /// <c>GET /v1/models</c> answers from configuration and
    /// <c>POST /v1/chat/completions</c> picks one connected workspace at random.</item>
    /// </list>
    /// </summary>
    public static partial class HttpForwardHandler
    {
        /// <summary>
        /// <c>GET /</c> - the connected workspace ids, as a JSON array (e.g.
        /// <c>["org/repo"]</c>), or <c>[]</c> when none is connected. Answered locally and
        /// without a credential: it is a discovery helper, not a proxy endpoint.
        /// </summary>
        public static Task HandleClientsAsync(HttpContext context)
        {
            var viewer = context.RequestServices.GetRequiredService<IClientViewer>();
            var clientIds = viewer
                .Select(client => client.Id)
                .OrderBy(id => id, StringComparer.Ordinal)
                .ToArray();

            return context.Response.WriteAsJsonAsync(clientIds, CnbJsonContext.Default.StringArray);
        }

        /// <summary>
        /// <c>GET /{org}/{repo}/v1/models</c> - answered locally from configuration, never
        /// forwarded. While that workspace's tunnel is down it answers 503, which is the
        /// keepalive cron's "fake alive" signal; either way it costs no AI quota.
        /// <para>
        /// The bare <c>GET /v1/models</c> shares this handler but deliberately does NOT look at
        /// the tunnel state: without a workspace prefix it is a static "what can this server
        /// serve" list, so it always answers 200.
        /// </para>
        /// </summary>
        public static Task HandleModelsAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var options = services.GetRequiredService<IOptions<CnbProxyOptions>>().Value;

            if (TryGetClientId(context, out var clientId))
            {
                var viewer = services.GetRequiredService<IClientViewer>();
                if (viewer.TryGetValue(clientId, out _) == false)
                {
                    LogWorkspaceNotConnected(
                        services.GetRequiredService<ILoggerFactory>().CreateLogger(LogCategory),
                        clientId,
                        StatusCodes.Status503ServiceUnavailable,
                        context.Request.Path.Value ?? "/");

                    context.Response.StatusCode = StatusCodes.Status503ServiceUnavailable;
                    return Task.CompletedTask;
                }
            }

            var models = options.ProxyModels is { Length: > 0 } configured ? configured : CnbProxyOptions.DefaultModels;
            var payload = new ModelListResponse
            {
                Data = models.Select(model => new ModelItem { Id = model, OwnedBy = "cnb" }).ToArray(),
            };

            return context.Response.WriteAsJsonAsync(payload, CnbJsonContext.Default.ModelListResponse);
        }

        /// <summary>
        /// <c>POST /{org}/{repo}/v1/chat/completions</c> - forwarded through the tunnel of that
        /// workspace. The caller's credential (the public ApiKey) is dropped and replaced with
        /// the CNB token the CYarp client reported during its handshake.
        /// <para>
        /// The bare <c>POST /v1/chat/completions</c> shares this handler but has no workspace
        /// prefix: it picks one of the connected workspaces at random (uniform), and answers 502
        /// when none is connected.
        /// </para>
        /// </summary>
        public static async Task HandleChatAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var logger = services.GetRequiredService<ILoggerFactory>().CreateLogger(LogCategory);

            IClient? client;
            if (TryGetClientId(context, out var clientId))
            {
                if (viewer.TryGetValue(clientId, out client) == false)
                {
                    LogWorkspaceNotConnected(logger, clientId, StatusCodes.Status502BadGateway, context.Request.Path.Value ?? "/");
                    context.Response.StatusCode = StatusCodes.Status502BadGateway;
                    return;
                }
            }
            else
            {
                client = PickRandomClient(viewer);
                if (client == null)
                {
                    LogNoConnectedWorkspace(logger, context.Request.Path.Value ?? "/");
                    context.Response.StatusCode = StatusCodes.Status502BadGateway;
                    return;
                }

                LogRandomRouted(logger, client.Id, viewer.Count);
            }

            // The CNB token travels in the handshake principal that CYarp keeps on IClient.User.
            var upstreamAuthorization = client.User.FindFirstValue(CnbClaims.UpstreamAuthorization);

            var stopwatch = Stopwatch.StartNew();
            var error = await client.ForwardHttpAsync(context, new CnbAiTransformer(upstreamAuthorization));
            stopwatch.Stop();

            if (error == Yarp.ReverseProxy.Forwarder.ForwarderError.None)
            {
                LogResponse(logger, client.Id, context.Request.Method, context.Response.StatusCode, stopwatch.Elapsed);
            }
            else
            {
                LogForwardError(logger, client.Id, context.Request.Method, error);
            }
        }

        /// <summary>
        /// Picks a connected workspace uniformly at random, or null when none is connected.
        /// The viewer is a concurrent collection, so the count is read once and indexed.
        /// </summary>
        private static IClient? PickRandomClient(IClientViewer viewer)
        {
            var count = viewer.Count;
            if (count == 0)
            {
                return null;
            }

            IClient? picked = null;
            var index = Random.Shared.Next(count);
            var current = 0;
            foreach (var candidate in viewer)
            {
                if (current++ == index)
                {
                    picked = candidate;
                    break;
                }
            }

            return picked;
        }

        /// <summary>
        /// Reads the workspace id from the <c>/{org}/{repo}</c> route values. Returns false for
        /// the bare (unified base_url) routes, which carry no workspace prefix.
        /// </summary>
        private static bool TryGetClientId(HttpContext context, out string clientId)
        {
            if (context.Request.RouteValues.TryGetValue("org", out var org) && org is string orgValue && orgValue.Length > 0
                && context.Request.RouteValues.TryGetValue("repo", out var repo) && repo is string repoValue && repoValue.Length > 0)
            {
                clientId = $"{orgValue}/{repoValue}";
                return true;
            }

            clientId = string.Empty;
            return false;
        }

        private const string LogCategory = "CYarpServer.HttpForward";

        [LoggerMessage(Level = LogLevel.Warning, Message = "Workspace '{clientId}' is not connected, {statusCode} returned for {path}")]
        private static partial void LogWorkspaceNotConnected(ILogger logger, string clientId, int statusCode, string path);

        [LoggerMessage(Level = LogLevel.Warning, Message = "No workspace is connected, 502 returned for {path}")]
        private static partial void LogNoConnectedWorkspace(ILogger logger, string path);

        [LoggerMessage(Level = LogLevel.Information, Message = "Randomly routed to workspace '{clientId}' out of {count} connected")]
        private static partial void LogRandomRouted(ILogger logger, string clientId, int count);

        [LoggerMessage(Level = LogLevel.Information, Message = "<= [{clientId}] {method} {statusCode}, elapsed {elapsed}")]
        private static partial void LogResponse(ILogger logger, string clientId, string method, int statusCode, TimeSpan elapsed);

        [LoggerMessage(Level = LogLevel.Error, Message = "=> [{clientId}] {method} forwarding failed with {error}")]
        private static partial void LogForwardError(ILogger logger, string clientId, string method, Yarp.ReverseProxy.Forwarder.ForwarderError error);
    }
}
