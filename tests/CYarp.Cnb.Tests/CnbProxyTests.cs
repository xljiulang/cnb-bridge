using CYarp.Client;
using CYarp.Server;
using CYarpServer;
using Microsoft.AspNetCore.Builder;
using Microsoft.Extensions.DependencyInjection;
using System;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Security.Claims;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// End to end tests: a real CYarp client tunnels to a real CYarp server, which
    /// forwards the public OpenAI-compatible surface to a stub upstream.
    /// <para>
    /// The workspace id is the CNB repo slug, so the public base url is
    /// <c>/{org}/{repo}/v1</c>.
    /// </para>
    /// </summary>
    public sealed class CnbProxyTests : IAsyncLifetime
    {
        private const string ProxyKey = "pk-test";
        private const string ChatBody = """{"model":"model-a","messages":[{"role":"user","content":"hi"}]}""";

        private readonly StubUpstream upstream = new();
        private readonly Dictionary<string, CYarp.Client.CYarpClient> tunnels = [];
        private readonly List<Task> tunnelTasks = [];
        private readonly CancellationTokenSource stopping = new();

        private WebApplication server = null!;
        private HttpClient publicClient = null!;
        private string serverAddress = null!;

        public async Task InitializeAsync()
        {
            await this.upstream.StartAsync();

            this.server = CYarpServerHost.Create(
                ["--urls", "http://127.0.0.1:0"],
                settings: new Dictionary<string, string?>
                {
                    ["ProxyKey"] = ProxyKey,
                    ["ProxyModels"] = "model-a,model-b",
                    ["Logging:LogLevel:Default"] = "Warning",
                });

            await this.server.StartAsync();

            this.serverAddress = StubUpstream.Address(this.server);
            this.publicClient = new HttpClient { BaseAddress = new Uri(this.serverAddress) };
        }

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
            await this.server.StopAsync();
            await this.server.DisposeAsync();
            await this.upstream.DisposeAsync();
            this.stopping.Dispose();
        }

        [Fact]
        public async Task TunnelHandshakeClaims_AreExposedOnIClientUser()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            var viewer = this.server.Services.GetRequiredService<IClientViewer>();
            Assert.True(viewer.TryGetValue("orgA/repoA", out var client));

            Assert.Equal("orgA/repoA", client.User.FindFirstValue(CnbClaims.ClientId));
            Assert.Equal("Bearer cnb-token-A", client.User.FindFirstValue(CnbClaims.UpstreamAuthorization));
            Assert.Equal(this.upstream.BaseAddress + "/orgA/repoA/-/ai", client.User.FindFirstValue(CnbClaims.TargetUri));
        }

        [Fact]
        public async Task SingleWorkspace_BarePath_IsForwardedWithTheCnbToken()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var response = await this.PostAsync("/v1/chat/completions?stream=false", ProxyKey);

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);

            var recorded = Assert.Single(this.upstream.Requests);
            Assert.Equal("POST", recorded.Method);
            Assert.Equal("/orgA/repoA/-/ai/chat/completions", recorded.Path);
            Assert.Equal("?stream=false", recorded.Query);
            Assert.Equal("Bearer cnb-token-A", recorded.Authorization);
            Assert.Contains("\"content\":\"hi\"", recorded.Body);
            Assert.DoesNotContain(ProxyKey, string.Join('\n', recorded.Headers.Values));

            // The caller's Host (our public domain/IP) must not reach the upstream: CNB's edge
            // routes by Host and answers "301 Moved Permanently -> https://cnb.cool" for hosts
            // it does not serve.
            Assert.Equal(new Uri(this.upstream.BaseAddress).Authority, recorded.Headers["Host"]);
        }

        [Fact]
        public async Task RepoSlugBaseUrl_RoutesToTheAddressedWorkspace()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            using var first = await this.PostAsync("/orgA/repoA/v1/chat/completions", ProxyKey);
            using var second = await this.PostAsync("/orgB/repoB/v1/chat/completions", ProxyKey);

            Assert.Equal(HttpStatusCode.OK, first.StatusCode);
            Assert.Equal(HttpStatusCode.OK, second.StatusCode);

            var recorded = this.upstream.Requests;
            Assert.Equal(2, recorded.Count);
            Assert.Contains(recorded, request => request.Path == "/orgA/repoA/-/ai/chat/completions" && request.Authorization == "Bearer cnb-token-A");
            Assert.Contains(recorded, request => request.Path == "/orgB/repoB/-/ai/chat/completions" && request.Authorization == "Bearer cnb-token-B");
        }

        [Fact]
        public async Task LongestConnectedPrefix_Wins()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgA/repoA/sub", "/orgA/repoA/sub/-/ai", "cnb-token-sub");

            using var nested = await this.PostAsync("/orgA/repoA/sub/v1/chat/completions", ProxyKey);
            using var parent = await this.PostAsync("/orgA/repoA/v1/chat/completions", ProxyKey);

            Assert.Equal(HttpStatusCode.OK, nested.StatusCode);
            Assert.Equal(HttpStatusCode.OK, parent.StatusCode);

            var recorded = this.upstream.Requests;
            Assert.Contains(recorded, request => request.Path == "/orgA/repoA/sub/-/ai/chat/completions" && request.Authorization == "Bearer cnb-token-sub");
            Assert.Contains(recorded, request => request.Path == "/orgA/repoA/-/ai/chat/completions" && request.Authorization == "Bearer cnb-token-A");
        }

        [Fact]
        public async Task SimilarPrefix_IsNotTreatedAsTheWorkspace()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            // "orgA/repository" must not match the connected workspace "orgA/repoA".
            using var response = await this.GetAsync("/orgA/repository/v1/models", ProxyKey);

            Assert.Equal(HttpStatusCode.ServiceUnavailable, response.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task TwoWorkspaces_BarePath_IsAmbiguousAndRejected()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            using var response = await this.PostAsync("/v1/chat/completions", ProxyKey);

            Assert.Equal(HttpStatusCode.BadGateway, response.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task UnknownWorkspacePrefix_IsRejectedAsBadGateway()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var response = await this.PostAsync("/ghost/repo/v1/chat/completions", ProxyKey);

            Assert.Equal(HttpStatusCode.BadGateway, response.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task ClientIdHeader_OverridesThePathPrefix()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            using var response = await this.PostAsync("/v1/chat/completions", ProxyKey, "orgB/repoB");

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            var recorded = Assert.Single(this.upstream.Requests);
            Assert.Equal("/orgB/repoB/-/ai/chat/completions", recorded.Path);
            Assert.Equal("Bearer cnb-token-B", recorded.Authorization);
        }

        [Fact]
        public async Task Models_BarePath_IsAnsweredLocallyWithoutAWorkspace()
        {
            using var response = await this.GetAsync("/v1/models", ProxyKey);
            using var trailingSlash = await this.GetAsync("/v1/models/", ProxyKey);

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            Assert.Equal(HttpStatusCode.OK, trailingSlash.StatusCode);

            var body = await response.Content.ReadAsStringAsync();
            Assert.Contains("\"object\":\"list\"", body);
            Assert.Contains("\"id\":\"model-a\"", body);
            Assert.Contains("\"object\":\"model\"", body);
            Assert.Contains("\"owned_by\":\"cnb\"", body);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task Models_WorkspacePrefix_ReflectsTheTunnelState()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var online = await this.GetAsync("/orgA/repoA/v1/models", ProxyKey);
            using var offline = await this.GetAsync("/ghost/repo/v1/models", ProxyKey);

            Assert.Equal(HttpStatusCode.OK, online.StatusCode);
            Assert.Contains("\"id\":\"model-a\"", await online.Content.ReadAsStringAsync());

            // Server up, tunnel dead: the signal the keepalive cron acts on.
            Assert.Equal(HttpStatusCode.ServiceUnavailable, offline.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task Models_ReportsOfflineAfterTheTunnelCloses()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using (var online = await this.GetAsync("/orgA/repoA/v1/models", ProxyKey))
            {
                Assert.Equal(HttpStatusCode.OK, online.StatusCode);
            }

            await this.DisconnectAsync("orgA/repoA");

            using var offline = await this.GetAsync("/orgA/repoA/v1/models", ProxyKey);
            Assert.Equal(HttpStatusCode.ServiceUnavailable, offline.StatusCode);
        }

        [Fact]
        public async Task LegacyTildeClientId_StillRoutes()
        {
            await this.ConnectAsync("orgA~repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var response = await this.PostAsync("/orgA~repoA/v1/chat/completions", ProxyKey);

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            var recorded = Assert.Single(this.upstream.Requests);
            Assert.Equal("/orgA/repoA/-/ai/chat/completions", recorded.Path);
        }

        [Fact]
        public async Task PublicPathUnderCyarpPrefix_StillRequiresTheProxyKey()
        {
            // A workspace id may start with "cyarp" (an org named cyarp): its public path looks
            // like "/cyarp/repo/v1/...", which must not be exempted from the ProxyKey check.
            using var withoutKey = await this.publicClient.GetAsync("/cyarp/repo/v1/models");
            Assert.Equal(HttpStatusCode.Unauthorized, withoutKey.StatusCode);

            using var withKey = await this.GetAsync("/cyarp/repo/v1/models", ProxyKey);
            Assert.Equal(HttpStatusCode.ServiceUnavailable, withKey.StatusCode);
        }

        [Fact]
        public async Task WrongProxyKey_IsRejectedBeforeReachingTheUpstream()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var wrong = await this.PostAsync("/v1/chat/completions", "pk-wrong");
            using var missing = await this.PostAsync("/v1/chat/completions", proxyKey: null);

            Assert.Equal(HttpStatusCode.Unauthorized, wrong.StatusCode);
            Assert.Equal(HttpStatusCode.Unauthorized, missing.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task Streaming_IsForwardedIncrementally()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var request = new HttpRequestMessage(HttpMethod.Post, "/orgA/repoA/v1/chat/completions")
            {
                Content = new StringContent("""{"model":"model-a","messages":[{"role":"user","content":"hi"}],"stream":true}""", Encoding.UTF8, "application/json"),
            };
            request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {ProxyKey}");

            using var response = await this.publicClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead);
            Assert.Equal(HttpStatusCode.OK, response.StatusCode);

            await using var stream = await response.Content.ReadAsStreamAsync();
            using var reader = new StreamReader(stream);

            // The stub holds the second frame back until this first one was observed,
            // so a buffering proxy would deadlock (and fail on timeout) instead.
            var firstFrame = await reader.ReadLineAsync();
            Assert.Equal("data: {\"chunk\":1}", firstFrame);

            this.upstream.ReleaseStream();

            Assert.Contains("data: {\"chunk\":2}", await reader.ReadToEndAsync());
        }

        private Task<HttpResponseMessage> PostAsync(string path, string? proxyKey, string? clientIdHeader = null)
        {
            var request = new HttpRequestMessage(HttpMethod.Post, path)
            {
                Content = new StringContent(ChatBody, Encoding.UTF8, "application/json"),
            };

            if (proxyKey != null)
            {
                request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {proxyKey}");
            }

            if (clientIdHeader != null)
            {
                request.Headers.TryAddWithoutValidation(CYarpServerHost.ClientIdHeader, clientIdHeader);
            }

            return this.publicClient.SendAsync(request);
        }

        private Task<HttpResponseMessage> GetAsync(string path, string proxyKey)
        {
            var request = new HttpRequestMessage(HttpMethod.Get, path);
            request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {proxyKey}");
            return this.publicClient.SendAsync(request);
        }

        private async Task ConnectAsync(string clientId, string targetPath, string cnbToken)
        {
            var options = new CYarpClientOptions
            {
                ServerUri = new Uri(this.serverAddress),
                TargetUri = new Uri(this.upstream.BaseAddress + targetPath),
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

        private async Task DisconnectAsync(string clientId)
        {
            Assert.True(this.tunnels.Remove(clientId, out var tunnel));
            tunnel.Dispose();

            await this.WaitForTunnelAsync(clientId, connected: false);
        }

        private async Task WaitForTunnelAsync(string clientId, bool connected)
        {
            var viewer = this.server.Services.GetRequiredService<IClientViewer>();
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
