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
    /// Handlers of the public OpenAI-compatible surface.
    /// <para>
    /// The routes live in <see cref="CYarpServerHost"/> and are literal:
    /// <c>GET /{org}/{repo}/v1/models</c> and
    /// <c>POST /{org}/{repo}/v1/chat/completions</c>. The workspace id is the CNB repo slug,
    /// built from those two route values, so no path parsing happens here.
    /// </para>
    /// </summary>
    public static partial class HttpForwardHandler
    {
        /// <summary>
        /// <c>GET /{org}/{repo}/v1/models</c> - answered locally from configuration, never
        /// forwarded. While that workspace's tunnel is down it answers 503, which is the
        /// keepalive cron's "fake alive" signal; either way it costs no AI quota.
        /// </summary>
        public static Task HandleModelsAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var options = services.GetRequiredService<IOptions<CnbProxyOptions>>().Value;
            var clientId = GetClientId(context);

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
        /// </summary>
        public static async Task HandleChatAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var logger = services.GetRequiredService<ILoggerFactory>().CreateLogger(LogCategory);
            var clientId = GetClientId(context);

            if (viewer.TryGetValue(clientId, out var client) == false)
            {
                LogWorkspaceNotConnected(logger, clientId, StatusCodes.Status502BadGateway, context.Request.Path.Value ?? "/");
                context.Response.StatusCode = StatusCodes.Status502BadGateway;
                return;
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
        /// Builds the workspace id ("org/repo") from the route values of
        /// <c>/{org}/{repo}/v1/...</c>. The route template guarantees both are present.
        /// </summary>
        private static string GetClientId(HttpContext context)
        {
            var org = context.Request.RouteValues["org"] as string ?? string.Empty;
            var repo = context.Request.RouteValues["repo"] as string ?? string.Empty;
            return $"{org}/{repo}";
        }

        private const string LogCategory = "CYarpServer.HttpForward";

        [LoggerMessage(Level = LogLevel.Warning, Message = "Workspace '{clientId}' is not connected, {statusCode} returned for {path}")]
        private static partial void LogWorkspaceNotConnected(ILogger logger, string clientId, int statusCode, string path);

        [LoggerMessage(Level = LogLevel.Information, Message = "<= [{clientId}] {method} {statusCode}, elapsed {elapsed}")]
        private static partial void LogResponse(ILogger logger, string clientId, string method, int statusCode, TimeSpan elapsed);

        [LoggerMessage(Level = LogLevel.Error, Message = "=> [{clientId}] {method} forwarding failed with {error}")]
        private static partial void LogForwardError(ILogger logger, string clientId, string method, Yarp.ReverseProxy.Forwarder.ForwarderError error);
    }
}
