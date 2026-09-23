using CYarp.Server;
using CYarpServer;
using CYarpServer.Forwarding;
using CYarpServer.Quota;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.DependencyInjection;
using System;
using System.Collections.Generic;
using System.Net.Http;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Shared harness of the AI quota tests: an in-process server, a real CYarp client per
    /// workspace and a stub upstream that also answers the two charge endpoints. Derived classes
    /// only declare the settings they vary and the cases they cover.
    /// </summary>
    public abstract class AiQuotaTestBench : IAsyncLifetime
    {
        private const string ApiKey = "pk-test";
        private const string ChatBody = """{"model":"model-a","messages":[{"role":"user","content":"hi"}]}""";

        private readonly Dictionary<string, CYarp.Client.CYarpClient> tunnels = [];
        private readonly List<Task> tunnelTasks = [];
        private readonly CancellationTokenSource stopping = new();

        private HttpClient publicClient = null!;
        private string serverAddress = null!;

        /// <summary>
        /// Gets a value indicating whether the server under test runs the probe and routes by quota.
        /// </summary>
        protected virtual bool QuotaRoutingEnabled => true;

        /// <summary>
        /// Gets the probe period of the server under test. One second by default, so the tests do
        /// not have to wait for the two minute production default.
        /// </summary>
        protected virtual string ProbeInterval => "00:00:01";

        /// <summary>
        /// Gets the stub that plays the CNB AI and charge endpoints.
        /// </summary>
        protected StubUpstream Upstream { get; } = new();

        /// <summary>
        /// Gets the running server.
        /// </summary>
        protected WebApplication Server { get; private set; } = null!;

        /// <summary>
        /// Gets the quota store of the running server.
        /// </summary>
        protected AiQuotaStore Store => this.Server.Services.GetRequiredService<AiQuotaStore>();

        /// <summary>
        /// Starts the stub, the server and the public HTTP client.
        /// </summary>
        public async Task InitializeAsync()
        {
            await this.Upstream.StartAsync();

            this.Server = CYarpServerHost.Create(
                ["--urls", "http://127.0.0.1:0"],
                settings: new Dictionary<string, string?>
                {
                    ["ApiKey"] = ApiKey,
                    ["ProxyModels:0"] = "model-a",
                    ["AiQuota:Enabled"] = this.QuotaRoutingEnabled ? "true" : "false",
                    ["AiQuota:Interval"] = this.ProbeInterval,
                    ["AiQuota:Timeout"] = "00:00:05",
                    ["AiQuota:StaleAfter"] = "00:00:30",
                    ["Logging:LogLevel:Default"] = "Warning",
                });

            await this.Server.StartAsync();

            this.serverAddress = StubUpstream.Address(this.Server);
            this.publicClient = new HttpClient { BaseAddress = new Uri(this.serverAddress) };
        }

        /// <summary>
        /// Tears everything down.
        /// </summary>
        public async Task DisposeAsync()
        {
            await this.stopping.CancelAsync();

            foreach (var tunnel in this.tunnels.Values)
            {
                tunnel.Dispose();
            }

            try
            {
                await Task.WhenAll(this.tunnelTasks);
            }
            catch (Exception)
            {
                // Tunnels are torn down on purpose.
            }

            this.publicClient.Dispose();
            await this.Server.StopAsync();
            await this.Server.DisposeAsync();
            await this.Upstream.DisposeAsync();
            this.stopping.Dispose();
        }

        /// <summary>
        /// Sends a bare chat request with the public ApiKey.
        /// </summary>
        /// <param name="path">Request path.</param>
        /// <returns>The response.</returns>
        protected Task<HttpResponseMessage> PostChatAsync(string path)
        {
            var request = new HttpRequestMessage(HttpMethod.Post, path)
            {
                Content = new StringContent(ChatBody, Encoding.UTF8, "application/json"),
            };
            request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {ApiKey}");

            return this.publicClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead);
        }

        /// <summary>
        /// Sends a GET, optionally with a bearer token.
        /// </summary>
        /// <param name="path">Request path.</param>
        /// <param name="apiKey">ApiKey to send, or null for an anonymous request.</param>
        /// <returns>The response.</returns>
        protected Task<HttpResponseMessage> GetAsync(string path, string? apiKey)
        {
            var request = new HttpRequestMessage(HttpMethod.Get, path);
            if (apiKey != null)
            {
                request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {apiKey}");
            }

            return this.publicClient.SendAsync(request);
        }

        /// <summary>
        /// Starts a real CYarp client for a workspace and waits until its tunnel is registered.
        /// </summary>
        /// <param name="clientId">The workspace id (the CNB repo slug).</param>
        /// <param name="targetPath">The tunnel target path (under the stub base address).</param>
        /// <param name="cnbToken">The CNB token to report in the handshake.</param>
        /// <returns>A task that completes once the tunnel is connected.</returns>
        protected async Task ConnectAsync(string clientId, string targetPath, string cnbToken)
        {
            await this.ConnectAsync(clientId, new Uri(this.Upstream.BaseAddress + targetPath), cnbToken);
        }

        /// <summary>
        /// Starts a real CYarp client whose tunnel target is given explicitly, so a test can point
        /// it at an address the workspace cannot reach.
        /// </summary>
        /// <param name="clientId">The workspace id (the CNB repo slug).</param>
        /// <param name="targetUri">The tunnel target.</param>
        /// <param name="cnbToken">The CNB token to report in the handshake.</param>
        /// <returns>A task that completes once the tunnel is connected.</returns>
        protected async Task ConnectAsync(string clientId, Uri targetUri, string cnbToken)
        {
            var options = new CYarp.Client.CYarpClientOptions
            {
                ServerUri = new Uri(this.serverAddress),
                TargetUri = targetUri,
                ConnectTimeout = TimeSpan.FromSeconds(10d),
                KeepAliveInterval = TimeSpan.FromSeconds(10d),
            };

            options.ConnectHeaders["Authorization"] = $"Bearer {cnbToken}";
            options.ConnectHeaders[CYarpServerHost.ClientIdHeader] = clientId;

            var tunnel = new CYarp.Client.CYarpClient(options);
            this.tunnels[clientId] = tunnel;
            this.tunnelTasks.Add(Task.Run(() => tunnel.TransportAsync(this.stopping.Token), this.stopping.Token));

            await this.WaitForTunnelAsync(clientId, connected: true);
        }

        /// <summary>
        /// Waits until the server has read the quota of a workspace.
        /// </summary>
        /// <param name="clientId">The workspace id.</param>
        /// <param name="timeout">How long to wait. The default suits the periodic probe; a test that
        /// asserts an immediate (connect-triggered) probe passes a short one on purpose.</param>
        /// <returns>The snapshot that carries the reading.</returns>
        protected async Task<AiQuotaSnapshot> WaitForQuotaAsync(string clientId, TimeSpan? timeout = null)
        {
            var deadline = DateTime.UtcNow + (timeout ?? TimeSpan.FromSeconds(30d));

            while (DateTime.UtcNow < deadline)
            {
                if (this.Store.TryGet(clientId, out var snapshot) && snapshot.RemainingMilliCredits is not null)
                {
                    return snapshot;
                }

                await Task.Delay(50);
            }

            Assert.Fail($"The quota of workspace '{clientId}' was expected to be read within {timeout ?? TimeSpan.FromSeconds(30d)}.");
            return null!;
        }

        /// <summary>
        /// Waits until the server has recorded a failed probe of a workspace.
        /// </summary>
        /// <param name="clientId">The workspace id.</param>
        /// <returns>A task that completes once a failure is recorded.</returns>
        protected async Task WaitForQuotaErrorAsync(string clientId)
        {
            var deadline = DateTime.UtcNow.AddSeconds(30d);

            while (DateTime.UtcNow < deadline)
            {
                if (this.Store.TryGet(clientId, out var snapshot) && snapshot.Error is not null)
                {
                    return;
                }

                await Task.Delay(50);
            }

            Assert.Fail($"A quota probe failure of workspace '{clientId}' was expected within 30s.");
        }

        private async Task WaitForTunnelAsync(string clientId, bool connected)
        {
            var viewer = this.Server.Services.GetRequiredService<IClientViewer>();
            var deadline = DateTime.UtcNow.AddSeconds(20d);

            while (DateTime.UtcNow < deadline)
            {
                if (viewer.TryGetValue(clientId, out _) == connected)
                {
                    return;
                }

                await Task.Delay(50);
            }

            Assert.Fail($"The tunnel of workspace '{clientId}' was expected to be {(connected ? "connected" : "gone")} within 20s.");
        }
    }
}
