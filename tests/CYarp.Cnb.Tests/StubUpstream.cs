using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.AspNetCore.Hosting.Server.Features;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using System;
using System.Collections.Generic;
using System.Collections.Concurrent;
using System.IO;
using System.Linq;
using System.Threading.Tasks;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Stands in for the CNB AI endpoint that a CYarp client tunnels to.
    /// Records what actually arrived, so tests can prove the path rewriting and the
    /// credential replacement happened on the server side.
    /// <para>
    /// It also answers the two charge endpoints on the same host, so a test can drive the quota
    /// the server probes through the tunnel.
    /// </para>
    /// </summary>
    public sealed class StubUpstream : IAsyncDisposable
    {
        private readonly WebApplication app;
        private readonly List<RecordedRequest> requests = [];
        private readonly List<RecordedRequest> chargeRequests = [];
        private readonly TaskCompletionSource streamGate = new(TaskCreationOptions.RunContinuationsAsynchronously);
        private readonly TaskCompletionSource chargeGate = new(TaskCreationOptions.RunContinuationsAsynchronously);
        private readonly ConcurrentDictionary<string, Quota> quotas = new(StringComparer.Ordinal);
        private int chargeFailureStatusCode = 0;
        private bool holdCharge = false;

        public StubUpstream()
        {
            var builder = WebApplication.CreateBuilder();
            builder.Logging.ClearProviders();
            builder.WebHost.UseUrls("http://127.0.0.1:0");

            this.app = builder.Build();
            this.app.Map("/{**catchall}", (RequestDelegate)this.HandleAsync);
        }

        public string BaseAddress { get; private set; } = string.Empty;

        public IReadOnlyList<RecordedRequest> Requests
        {
            get
            {
                lock (this.requests)
                {
                    return this.requests.ToArray();
                }
            }
        }

        /// <summary>
        /// The charge requests the server's quota probe sent through a tunnel. Kept apart from
        /// <see cref="Requests"/> so the assertions about the forwarded chat traffic stay exact.
        /// </summary>
        public IReadOnlyList<RecordedRequest> ChargeRequests
        {
            get
            {
                lock (this.chargeRequests)
                {
                    return this.chargeRequests.ToArray();
                }
            }
        }

        public async Task StartAsync()
        {
            await this.app.StartAsync();
            this.BaseAddress = Address(this.app);
        }

        /// <summary>
        /// Sets the monthly credit allowance and the usage of one organization, in milli-credits.
        /// The server reads the remaining amount as total - used - frozen.
        /// </summary>
        public void SetQuota(string organization, long totalMilliCredits, long usedMilliCredits, long frozenMilliCredits = 0L)
        {
            this.quotas[organization] = new Quota(totalMilliCredits, usedMilliCredits, frozenMilliCredits);
        }

        /// <summary>
        /// Makes both charge endpoints answer with the given failure status; 0 restores the
        /// successful behaviour. The CNB charge endpoints need the group-resource:r permission,
        /// so 401/403 is the realistic failure mode.
        /// </summary>
        public void SetChargeFailure(int statusCode)
        {
            this.chargeFailureStatusCode = statusCode;
        }

        /// <summary>
        /// Holds every charge response until <see cref="ReleaseCharge"/> is called, so a test can
        /// observe the server while a quota request is in flight.
        /// </summary>
        public void HoldCharge()
        {
            this.holdCharge = true;
        }

        /// <summary>
        /// Lets held charge responses complete.
        /// </summary>
        public void ReleaseCharge()
        {
            this.chargeGate.TrySetResult();
        }

        /// <summary>
        /// Lets a gated streaming response continue after the test observed its first frame.
        /// </summary>
        public void ReleaseStream()
        {
            this.streamGate.TrySetResult();
        }

        private async Task HandleAsync(HttpContext context)
        {
            string body;
            using (var reader = new StreamReader(context.Request.Body))
            {
                body = await reader.ReadToEndAsync();
            }

            var path = context.Request.Path.Value ?? "/";
            var headers = context.Request.Headers.ToDictionary(header => header.Key, header => header.Value.ToString(), StringComparer.OrdinalIgnoreCase);
            var recorded = new RecordedRequest(
                context.Request.Method,
                path,
                context.Request.QueryString.Value ?? string.Empty,
                context.Request.Headers.Authorization.ToString(),
                headers,
                body);

            var isCharge = IsChargePath(path);
            lock (isCharge ? this.chargeRequests : this.requests)
            {
                if (isCharge)
                {
                    this.chargeRequests.Add(recorded);
                }
                else
                {
                    this.requests.Add(recorded);
                }
            }

            context.Response.ContentType = "application/json";

            if (isCharge == true && await this.TryHandleChargeAsync(context) == true)
            {
                return;
            }

            if (body.Contains("\"stream\":true", StringComparison.Ordinal))
            {
                // First frame, then block until the test has seen it: a proxy that buffers
                // the whole response can never deliver it.
                await context.Response.WriteAsync("data: {\"chunk\":1}\n\n");
                await context.Response.Body.FlushAsync();
                await this.streamGate.Task;
                await context.Response.WriteAsync("data: {\"chunk\":2}\n\n");
                await context.Response.Body.FlushAsync();
                return;
            }

            await context.Response.WriteAsync("""{"id":"stub-chatcmpl","object":"chat.completion","model":"model-a"}""");
        }

        /// <summary>
        /// Answers <c>/{org}/-/charge/quota</c> and <c>/{org}/-/charge/volume</c> with the
        /// fields the server reads. Returns false for every other path.
        /// </summary>
        private async Task<bool> TryHandleChargeAsync(HttpContext context)
        {
            var segments = (context.Request.Path.Value ?? "/").Split('/', StringSplitOptions.RemoveEmptyEntries);
            if (IsChargePath(context.Request.Path.Value) == false)
            {
                return false;
            }

            if (this.chargeFailureStatusCode != 0)
            {
                context.Response.StatusCode = this.chargeFailureStatusCode;
                return true;
            }

            if (this.holdCharge)
            {
                await this.chargeGate.Task;
            }

            var quota = this.quotas.TryGetValue(segments[0], out var configured) ? configured : Quota.Empty;

            if (segments[3] == "quota")
            {
                var total = quota.TotalMilliCredits;
                await context.Response.WriteAsync($"{{\"credit_in_milli\":{{\"free\":0,\"total\":{total}}}}}");
                return true;
            }

            if (segments[3] == "volume")
            {
                var used = quota.UsedMilliCredits;
                var frozen = quota.FrozenMilliCredits;
                await context.Response.WriteAsync($"{{\"credit_in_milli\":{used},\"freeze_credit_in_milli\":{frozen}}}");
                return true;
            }

            return false;
        }

        /// <summary>
        /// Gets whether a path addresses a charge endpoint, i.e. <c>/{org}/-/charge/{kind}</c>.
        /// </summary>
        private static bool IsChargePath(string? path)
        {
            var segments = (path ?? "/").Split('/', StringSplitOptions.RemoveEmptyEntries);
            return segments.Length == 4 && segments[1] == "-" && segments[2] == "charge";
        }

        public async ValueTask DisposeAsync()
        {
            this.streamGate.TrySetResult();
            this.chargeGate.TrySetResult();
            await this.app.StopAsync();
            await this.app.DisposeAsync();
        }

        /// <summary>
        /// Reads the address a started application is listening on.
        /// </summary>
        /// <param name="app">The started application.</param>
        /// <returns>The first listening address.</returns>
        public static string Address(WebApplication app)
        {
            return app.Services.GetRequiredService<IServer>().Features.Get<IServerAddressesFeature>()!.Addresses.First();
        }

        /// <summary>
        /// A request the stub received.
        /// </summary>
        /// <param name="Method">HTTP method.</param>
        /// <param name="Path">Request path.</param>
        /// <param name="Query">Query string, including the leading "?".</param>
        /// <param name="Authorization">Authorization header.</param>
        /// <param name="Headers">All headers.</param>
        /// <param name="Body">Request body.</param>
        public sealed record RecordedRequest(
            string Method,
            string Path,
            string Query,
            string Authorization,
            IReadOnlyDictionary<string, string> Headers,
            string Body);

        /// <summary>
        /// The quota of one organization the stub reports, in milli-credits.
        /// </summary>
        /// <param name="TotalMilliCredits">Monthly allowance.</param>
        /// <param name="UsedMilliCredits">Credits already spent this month.</param>
        /// <param name="FrozenMilliCredits">Credits frozen for in-flight tasks.</param>
        public readonly record struct Quota(long TotalMilliCredits, long UsedMilliCredits, long FrozenMilliCredits)
        {
            /// <summary>
            /// Gets a quota with no allowance and no usage.
            /// </summary>
            public static Quota Empty { get; } = new(0L, 0L, 0L);
        }
    }
}
