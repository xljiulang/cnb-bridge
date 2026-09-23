using CYarp.Server;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Security.Claims;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using System.Threading;
using System.Threading.Tasks;

namespace CYarpServer
{
    /// <summary>
    /// Reads the CNB AI quota of every connected workspace, periodically.
    /// <para>
    /// The charge endpoints only answer requests that originate inside the CNB network, so the
    /// probe does not call them from the server: it asks each workspace for a TCP tunnel
    /// (<see cref="IClient.CreateTcpTunnelAsync"/>) and runs a plain HTTPS request over that
    /// tunnel. TLS and HTTP are handled by <see cref="SocketsHttpHandler"/> on top of the tunnel
    /// stream, exactly the way CYarp itself forwards HTTP requests.
    /// </para>
    /// <para>
    /// Nothing here is allowed to affect the tunnel: a failure is stored as the snapshot error and
    /// the previous value is kept, so the worst case is a workspace whose quota stays unknown.
    /// </para>
    /// </summary>
    public sealed partial class AiQuotaProbeService : BackgroundService
    {
        /// <summary>
        /// Media type of the CNB OpenAPI; the charge endpoints do not advertise <c>application/json</c>.
        /// </summary>
        private const string AcceptMediaType = "application/vnd.cnb.api+json";

        private readonly IClientViewer _viewer;
        private readonly AiQuotaStore _store;
        private readonly AiQuotaOptions _options;
        private readonly ILogger<AiQuotaProbeService> _logger;

        /// <summary>
        /// Creates the probe.
        /// </summary>
        /// <param name="viewer">The connected workspaces.</param>
        /// <param name="store">Where the readings are kept for the routing and for <c>GET /</c>.</param>
        /// <param name="options">Probe settings.</param>
        /// <param name="logger">Logger.</param>
        public AiQuotaProbeService(
            IClientViewer viewer,
            AiQuotaStore store,
            IOptions<AiQuotaOptions> options,
            ILogger<AiQuotaProbeService> logger)
        {
            this._viewer = viewer;
            this._store = store;
            this._options = options.Value;
            this._logger = logger;
        }

        /// <summary>
        /// Probing loop: one round immediately, then one per <see cref="AiQuotaOptions.Interval"/>.
        /// A round is awaited before the next tick, so rounds never overlap and a slow workspace
        /// cannot pile up work.
        /// </summary>
        /// <param name="stoppingToken">Signalled when the host shuts down.</param>
        protected override async Task ExecuteAsync(CancellationToken stoppingToken)
        {
            if (this._options.Enabled == false)
            {
                LogProbeDisabled(this._logger);
                return;
            }

            if (this._options.Interval <= TimeSpan.Zero)
            {
                LogProbeIntervalInvalid(this._logger, this._options.Interval);
                return;
            }

            try
            {
                using var timer = new PeriodicTimer(this._options.Interval);
                await this.ProbeAllAsync(stoppingToken);

                while (await timer.WaitForNextTickAsync(stoppingToken))
                {
                    await this.ProbeAllAsync(stoppingToken);
                }
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                // Shutting down.
            }
        }

        /// <summary>
        /// Probes every connected workspace once, sequentially.
        /// <para>
        /// Sequential on purpose: workspaces of the same organization share one quota, so probing
        /// them in parallel would only multiply the load on the same endpoint.
        /// </para>
        /// </summary>
        /// <param name="cancellationToken">Signalled when the host shuts down.</param>
        private async Task ProbeAllAsync(CancellationToken cancellationToken)
        {
            // The viewer is a concurrent collection; snapshot it so one probe round sees a stable
            // set of workspaces.
            var candidates = new List<IClient>(this._viewer.Count);
            foreach (var client in this._viewer)
            {
                candidates.Add(client);
            }

            foreach (var client in candidates)
            {
                cancellationToken.ThrowIfCancellationRequested();
                await this.ProbeOneAsync(client, cancellationToken);
            }
        }

        /// <summary>
        /// Probes one workspace. Every failure is recorded on the snapshot instead of being
        /// thrown, so one broken workspace cannot stop the round.
        /// </summary>
        /// <param name="client">The workspace to probe.</param>
        /// <param name="cancellationToken">Signalled when the host shuts down.</param>
        private async Task ProbeOneAsync(IClient client, CancellationToken cancellationToken)
        {
            var clientId = client.Id;

            // The organization is the first segment of the repo slug. A client id without a
            // slash (the single workspace default) carries no organization, so there is nothing
            // to ask for.
            var separator = clientId.IndexOf('/');
            if (separator <= 0)
            {
                LogProbeSkipped(this._logger, clientId, "the client id has no organization");
                return;
            }

            // Without the CNB token there is no way to authenticate the charge call, and the
            // server never invents one.
            var authorization = client.User.FindFirstValue(CnbClaims.UpstreamAuthorization);
            if (string.IsNullOrEmpty(authorization))
            {
                LogProbeSkipped(this._logger, clientId, "the handshake carried no upstream authorization");
                return;
            }

            var organization = clientId[..separator];

            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            timeout.CancelAfter(this._options.Timeout);

            try
            {
                var remaining = await ReadRemainingMilliCreditsAsync(client, organization, authorization, timeout.Token);
                var now = DateTimeOffset.UtcNow;

                this._store.ReportSuccess(clientId, remaining, now);
                LogProbeSucceeded(this._logger, clientId, organization, remaining);
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
            {
                throw;
            }
            catch (Exception ex)
            {
                var error = Describe(ex);
                this._store.ReportFailure(clientId, error);
                LogProbeFailed(this._logger, clientId, organization, error);
            }
        }

        /// <summary>
        /// Reads the remaining credits of one organization over the workspace's own tunnel.
        /// </summary>
        /// <param name="client">The workspace that provides the tunnel.</param>
        /// <param name="organization">The organization slug (first segment of the client id).</param>
        /// <param name="authorization">The CNB token reported during the handshake.</param>
        /// <param name="cancellationToken">Timeout of the two requests together.</param>
        /// <returns>Remaining credits in milli-credits.</returns>
        private static async Task<long> ReadRemainingMilliCreditsAsync(
            IClient client,
            string organization,
            string authorization,
            CancellationToken cancellationToken)
        {
            // The target Uri of the tunnel is the CNB AI base (https://api.cnb.cool/{org}/{repo}/-/ai);
            // the charge endpoints live on the same host, so no extra address is configured.
            var target = client.TargetUri;
            var baseAddress = new Uri($"{target.Scheme}://{target.Authority}");

            var handler = new SocketsHttpHandler
            {
                Proxy = null,
                UseProxy = false,
                UseCookies = false,
                AllowAutoRedirect = false,
                // One connection is enough for the two requests, and keeping it at one means the
                // round holds a single tunnel instead of one per request.
                MaxConnectionsPerServer = 1,
                ConnectCallback = (_, token) => new ValueTask<Stream>(client.CreateTcpTunnelAsync(token)),
            };

            // The invoker is created per workspace and per round: it must never be shared, or a
            // pooled connection would be reused for the tunnel of another workspace.
            using var invoker = new HttpMessageInvoker(handler, disposeHandler: true);

            var escapedOrganization = Uri.EscapeDataString(organization);
            var quotaPath = $"/{escapedOrganization}/-/charge/quota";
            var volumePath = $"/{escapedOrganization}/-/charge/volume";

            var quota = await GetAsync(
                invoker,
                new Uri(baseAddress, quotaPath),
                quotaPath,
                authorization,
                CnbJsonContext.Default.CnbChargeQuotaResponse,
                cancellationToken);

            var volume = await GetAsync(
                invoker,
                new Uri(baseAddress, volumePath),
                volumePath,
                authorization,
                CnbJsonContext.Default.CnbChargeVolumeResponse,
                cancellationToken);

            if (quota.CreditInMilli?.Total is not { } total)
            {
                throw new InvalidOperationException($"{quotaPath} carried no credit_in_milli.total");
            }

            if (volume.CreditInMilli is not { } used)
            {
                throw new InvalidOperationException($"{volumePath} carried no credit_in_milli");
            }

            var frozen = volume.FreezeCreditInMilli ?? 0L;
            return total - used - frozen;
        }

        /// <summary>
        /// Sends one authenticated GET over the tunnel and reads its JSON body. A non success
        /// status, an unreadable body or a missing body is reported as an exception, which the
        /// caller turns into the snapshot error.
        /// </summary>
        /// <typeparam name="TResponse">The part of the response this server reads.</typeparam>
        /// <param name="invoker">The per workspace invoker that owns the tunnel.</param>
        /// <param name="requestUri">Absolute request Uri.</param>
        /// <param name="path">Path, used in error messages.</param>
        /// <param name="authorization">The CNB token reported during the handshake.</param>
        /// <param name="typeInfo">Source generated JSON metadata (the publish is trimmed).</param>
        /// <param name="cancellationToken">Request timeout.</param>
        /// <returns>The deserialized response.</returns>
        private static async Task<TResponse> GetAsync<TResponse>(
            HttpMessageInvoker invoker,
            Uri requestUri,
            string path,
            string authorization,
            JsonTypeInfo<TResponse> typeInfo,
            CancellationToken cancellationToken)
            where TResponse : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, requestUri);
            request.Headers.TryAddWithoutValidation("Accept", AcceptMediaType);
            request.Headers.TryAddWithoutValidation("Authorization", authorization);

            using var response = await invoker.SendAsync(request, cancellationToken);
            if (response.IsSuccessStatusCode == false)
            {
                throw new HttpRequestException(
                    $"GET {path} returned {(int)response.StatusCode} {response.ReasonPhrase}",
                    inner: null,
                    response.StatusCode);
            }

            await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken);
            var payload = await JsonSerializer.DeserializeAsync(stream, typeInfo, cancellationToken);
            if (payload is null)
            {
                throw new InvalidOperationException($"GET {path} returned an empty body");
            }

            return payload;
        }

        /// <summary>
        /// Renders an exception as a single line error message for the snapshot.
        /// </summary>
        /// <param name="exception">The failure.</param>
        /// <returns>A short description, including the HTTP status when there is one.</returns>
        private static string Describe(Exception exception)
        {
            if (exception is HttpRequestException httpException && httpException.StatusCode is { } statusCode)
            {
                return $"{(int)statusCode} {httpException.Message}";
            }

            if (exception is OperationCanceledException)
            {
                return "the request timed out";
            }

            return exception.Message;
        }

        [LoggerMessage(Level = LogLevel.Information, Message = "AI quota probe is disabled (AiQuota:Enabled=false)")]
        private static partial void LogProbeDisabled(ILogger logger);

        [LoggerMessage(Level = LogLevel.Warning, Message = "AI quota probe is disabled: AiQuota:Interval={interval} is not positive")]
        private static partial void LogProbeIntervalInvalid(ILogger logger, TimeSpan interval);

        [LoggerMessage(Level = LogLevel.Debug, Message = "AI quota probe skipped workspace '{clientId}': {reason}")]
        private static partial void LogProbeSkipped(ILogger logger, string clientId, string reason);

        [LoggerMessage(Level = LogLevel.Information, Message = "AI quota of workspace '{clientId}' (organization {organization}): {remainingMilliCredits} milli-credits remaining")]
        private static partial void LogProbeSucceeded(ILogger logger, string clientId, string organization, long remainingMilliCredits);

        [LoggerMessage(Level = LogLevel.Warning, Message = "AI quota probe of workspace '{clientId}' (organization {organization}) failed: {error}")]
        private static partial void LogProbeFailed(ILogger logger, string clientId, string organization, string error);
    }
}
