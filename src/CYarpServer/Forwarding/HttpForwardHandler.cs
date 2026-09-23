using CYarp.Server;
using CYarpServer.Authentication;
using CYarpServer.Contracts;
using CYarpServer.Options;
using CYarpServer.Quota;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Security.Claims;
using System.Threading.Tasks;

namespace CYarpServer.Forwarding
{
    /// <summary>
    /// Handlers of the public OpenAI-compatible surface. The routes live in
    /// <see cref="CYarpServerHost"/> and come in two flavours:
    /// <list type="bullet">
    /// <item>workspace-addressed: <c>/{org}/{repo}/v1/models</c> and
    /// <c>/{org}/{repo}/v1/chat/completions</c> - the workspace id is the CNB repo slug, built
    /// from the two route values, so no path parsing happens here;</item>
    /// <item>bare (unified base_url): <c>GET /</c> lists the connected workspaces and their AI
    /// quota,
    /// <c>GET /v1/models</c> answers from configuration and
    /// <c>POST /v1/chat/completions</c> picks the connected workspace with the most remaining
    /// AI credits.</item>
    /// </list>
    /// </summary>
    public static partial class HttpForwardHandler
    {
        /// <summary>
        /// Offset every timestamp of the response is rendered in (<see cref="ClientInfo.ConnectedAt"/>
        /// and <see cref="ClientInfo.UpdatedAt"/>). Fixed on purpose: the host's local zone differs
        /// between a Windows machine in UTC+8 and a minimal Linux image that defaults to UTC, so
        /// following the host would make the same field mean different wall clocks per deployment.
        /// </summary>
        private static readonly TimeSpan ResponseTimeOffset = TimeSpan.FromHours(8d);

        /// <summary>
        /// Normalises a timestamp of this response to its fixed <c>+08:00</c> offset, keeping the
        /// instant unchanged. Applied to every time the response carries, so a caller never has to
        /// reconcile two different offsets in one payload.
        /// <para>
        /// Kept as its own method because the distinction it makes is invisible on a UTC+8 host and
        /// only shows up elsewhere: <see cref="DateTimeOffset.ToOffset"/> moves the wall clock along
        /// with the suffix (03:29Z becomes 11:29+08:00), whereas rebuilding the value from its
        /// <c>DateTime</c> would relabel the same wall clock and silently shift the instant by the
        /// difference between the host zone and the target zone.
        /// </para>
        /// </summary>
        /// <param name="time">The timestamp, in whatever offset it carries.</param>
        /// <returns>The same instant, expressed in +08:00.</returns>
        internal static DateTimeOffset ToResponseTime(DateTimeOffset time)
        {
            return time.ToOffset(ResponseTimeOffset);
        }

        /// <summary>
        /// <c>GET /</c> - the connected workspaces with the AI quota last read for each of them,
        /// as a JSON array of <see cref="ClientInfo"/>, or <c>[]</c> when none is connected.
        /// Answered locally and without a credential: it is a discovery helper, not a proxy
        /// endpoint.
        /// <para>
        /// Only connected workspaces are listed, and the quota value is the last one the probe
        /// managed to read: <see cref="ClientInfo.Stale"/> and <see cref="ClientInfo.Error"/>
        /// describe whether routing still trusts it.
        /// </para>
        /// </summary>
        public static Task HandleClientsAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var store = services.GetRequiredService<AiQuotaStore>();
            var options = services.GetRequiredService<IOptions<AiQuotaOptions>>().Value;
            var now = DateTimeOffset.UtcNow;

            var clients = viewer
                .OrderBy(client => client.Id, StringComparer.Ordinal)
                .Select(client => ToClientInfo(client, store, options, now))
                .ToArray();

            return context.Response.WriteAsJsonAsync(clients, CnbJsonContext.Default.ClientInfoArray);
        }

        /// <summary>
        /// Projects one connected workspace and the quota probed for it into the response object.
        /// The tunnel is live by definition here - only connected workspaces are listed - so the
        /// connection is reported as the moment it was established.
        /// </summary>
        /// <param name="client">The connected workspace.</param>
        /// <param name="store">The probed quota.</param>
        /// <param name="options">Probe settings, for the staleness threshold.</param>
        /// <param name="now">Current time.</param>
        /// <returns>The response object of that workspace.</returns>
        internal static ClientInfo ToClientInfo(IClient client, AiQuotaStore store, AiQuotaOptions options, DateTimeOffset now)
        {
            var clientId = client.Id;

            var connectedAt = ToResponseTime(client.CreationTime);

            if (store.TryGet(clientId, out var snapshot) == false)
            {
                return new ClientInfo
                {
                    ClientId = clientId,
                    ConnectedAt = connectedAt,
                    RemainingCredits = null,
                    TotalCredits = null,
                    Stale = true,
                    UpdatedAt = null,
                    Error = null,
                };
            }

            return new ClientInfo
            {
                ClientId = clientId,
                ConnectedAt = connectedAt,
                RemainingCredits = snapshot.RemainingMilliCredits is { } remaining ? remaining / 1000m : null,
                TotalCredits = snapshot.TotalMilliCredits is { } total ? total / 1000m : null,
                Stale = snapshot.IsStale(now, options.StaleAfter),
                UpdatedAt = snapshot.UpdatedAt is { } updatedAt ? ToResponseTime(updatedAt) : null,
                Error = snapshot.Error,
            };
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
        /// prefix: it picks the connected workspace with the most remaining AI credits (see
        /// <see cref="AiQuotaRouter.Pick"/>), and answers 502 when none is connected.
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
                var options = services.GetRequiredService<IOptions<AiQuotaOptions>>().Value;
                var store = services.GetRequiredService<AiQuotaStore>();
                var selection = AiQuotaRouter.Pick(
                    Snapshot(viewer),
                    store,
                    options.Enabled,
                    DateTimeOffset.UtcNow,
                    options.StaleAfter,
                    Random.Shared);

                if (selection is not { } picked)
                {
                    LogNoConnectedWorkspace(logger, context.Request.Path.Value ?? "/");
                    context.Response.StatusCode = StatusCodes.Status502BadGateway;
                    return;
                }

                client = picked.Client;
                LogRouted(logger, picked.Client.Id, picked.Tier, picked.RemainingMilliCredits, viewer.Count);
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
        /// Copies the connected workspaces into a list, so the routing decision works on a stable
        /// set even though the viewer is a collection that mutates as tunnels come and go.
        /// </summary>
        /// <param name="viewer">The connected workspaces.</param>
        /// <returns>The workspace snapshot.</returns>
        private static IReadOnlyList<IClient> Snapshot(IClientViewer viewer)
        {
            var clients = new List<IClient>(viewer.Count);
            foreach (var client in viewer)
            {
                clients.Add(client);
            }

            return clients;
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

        [LoggerMessage(Level = LogLevel.Information, Message = "Routed to workspace '{clientId}' ({tier}, {remainingMilliCredits} milli-credits remaining) out of {count} connected")]
        private static partial void LogRouted(ILogger logger, string clientId, AiQuotaTier tier, long? remainingMilliCredits, int count);

        [LoggerMessage(Level = LogLevel.Information, Message = "<= [{clientId}] {method} {statusCode}, elapsed {elapsed}")]
        private static partial void LogResponse(ILogger logger, string clientId, string method, int statusCode, TimeSpan elapsed);

        [LoggerMessage(Level = LogLevel.Error, Message = "=> [{clientId}] {method} forwarding failed with {error}")]
        private static partial void LogForwardError(ILogger logger, string clientId, string method, Yarp.ReverseProxy.Forwarder.ForwarderError error);
    }
}
