using CYarp.Client;
using CYarp.Server;
using CYarpServer;
using CYarpServer.Authentication;
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
    /// End to end tests: a real CYarp client tunnels to a real CYarp server, which forwards
    /// the public OpenAI-compatible surface to a stub upstream.
    /// <para>
    /// The published surface is a literal route table in two flavours: workspace-addressed
    /// (<c>/{org}/{repo}/v1/models</c>, <c>/{org}/{repo}/v1/chat/completions</c>) and bare
    /// (<c>GET /</c>, <c>GET /v1/models</c>, <c>POST /v1/chat/completions</c>); everything else is 404.
    /// </para>
    /// </summary>
    public sealed class CnbProxyTests : IAsyncLifetime
    {
        private const string ApiKey = "pk-test";
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
                    ["ApiKey"] = ApiKey,
                    ["ProxyModels:0"] = "model-a",
                    ["ProxyModels:1"] = "model-b",
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
        public async Task Chat_IsForwardedWithTheCnbToken()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var response = await this.PostChatAsync("/orgA/repoA/v1/chat/completions?stream=false");

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);

            var recorded = Assert.Single(this.upstream.Requests);
            Assert.Equal("POST", recorded.Method);
            Assert.Equal("/orgA/repoA/-/ai/chat/completions", recorded.Path);
            Assert.Equal("?stream=false", recorded.Query);
            Assert.Equal("Bearer cnb-token-A", recorded.Authorization);
            Assert.Contains("\"content\":\"hi\"", recorded.Body);
            Assert.DoesNotContain(ApiKey, string.Join('\n', recorded.Headers.Values));

            // The caller's Host (our public domain/IP) must not reach the upstream: CNB's edge
            // routes by Host and answers "301 Moved Permanently -> https://cnb.cool" for hosts
            // it does not serve.
            Assert.Equal(new Uri(this.upstream.BaseAddress).Authority, recorded.Headers["Host"]);
        }

        [Fact]
        public async Task TwoWorkspaces_RouteIndependently()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            using var first = await this.PostChatAsync("/orgA/repoA/v1/chat/completions");
            using var second = await this.PostChatAsync("/orgB/repoB/v1/chat/completions");

            Assert.Equal(HttpStatusCode.OK, first.StatusCode);
            Assert.Equal(HttpStatusCode.OK, second.StatusCode);

            var recorded = this.upstream.Requests;
            Assert.Equal(2, recorded.Count);
            Assert.Contains(recorded, request => request.Path == "/orgA/repoA/-/ai/chat/completions" && request.Authorization == "Bearer cnb-token-A");
            Assert.Contains(recorded, request => request.Path == "/orgB/repoB/-/ai/chat/completions" && request.Authorization == "Bearer cnb-token-B");
        }

        [Fact]
        public async Task Models_AreAnsweredLocallyFromConfiguration()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var response = await this.GetAsync("/orgA/repoA/v1/models");
            using var trailingSlash = await this.GetAsync("/orgA/repoA/v1/models/");

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            Assert.Equal(HttpStatusCode.OK, trailingSlash.StatusCode);

            var body = await response.Content.ReadAsStringAsync();
            Assert.Contains("\"object\":\"list\"", body);
            Assert.Contains("\"id\":\"model-a\"", body);
            Assert.Contains("\"id\":\"model-b\"", body);
            Assert.Contains("\"owned_by\":\"cnb\"", body);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task Models_ReportOfflineWhileTheTunnelIsDown()
        {
            // Never connected: the workspace is unknown, which is the same signal as a dead tunnel.
            using var offline = await this.GetAsync("/ghost/repo/v1/models");
            Assert.Equal(HttpStatusCode.ServiceUnavailable, offline.StatusCode);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            using (var online = await this.GetAsync("/orgA/repoA/v1/models"))
            {
                Assert.Equal(HttpStatusCode.OK, online.StatusCode);
            }

            await this.DisconnectAsync("orgA/repoA");

            using var afterDisconnect = await this.GetAsync("/orgA/repoA/v1/models");
            Assert.Equal(HttpStatusCode.ServiceUnavailable, afterDisconnect.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task Chat_OfAnOfflineWorkspace_IsBadGateway()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var response = await this.PostChatAsync("/ghost/repo/v1/chat/completions");

            Assert.Equal(HttpStatusCode.BadGateway, response.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task RootPath_ListsTheConnectedWorkspacesAsObjects()
        {
            using (var empty = await this.GetAsync("/", apiKey: null))
            {
                Assert.Equal(HttpStatusCode.OK, empty.StatusCode);
                Assert.Equal("[]", await empty.Content.ReadAsStringAsync());
            }

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            using (var both = await this.GetAsync("/", apiKey: null))
            {
                Assert.Equal(HttpStatusCode.OK, both.StatusCode);

                // Objects, not ids: this list carries the probed AI quota of every workspace now.
                // The quota itself is covered by the AI quota tests; here only the shape and the
                // ordering matter. Neither workspace has been probed in this test, so both quotas
                // are unknown and the tie falls back to the clientId order - which is why orgA
                // comes first rather than by any luck of the quota sort.
                var body = await both.Content.ReadAsStringAsync();
                Assert.StartsWith("""[{"clientId":"orgA/repoA","connectedAt":""", body);
                Assert.Contains("""{"clientId":"orgB/repoB","connectedAt":""", body);
            }

            await this.DisconnectAsync("orgA/repoA");

            using var remaining = await this.GetAsync("/", apiKey: null);
            var remainingBody = await remaining.Content.ReadAsStringAsync();
            Assert.StartsWith("""[{"clientId":"orgB/repoB","connectedAt":""", remainingBody);
            Assert.DoesNotContain("orgA/repoA", remainingBody);
        }

        [Fact]
        public async Task BareModels_ReturnsTheConfiguredListWithoutAnyWorkspace()
        {
            // No workspace connected at all: the bare list is a static "what can this server
            // serve" answer and must not depend on the tunnel state (unlike /{org}/{repo}/v1/models).
            using var response = await this.GetAsync("/v1/models", apiKey: null);

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);

            var body = await response.Content.ReadAsStringAsync();
            Assert.Contains("\"object\":\"list\"", body);
            Assert.Contains("\"id\":\"model-a\"", body);
            Assert.Contains("\"id\":\"model-b\"", body);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task BareChat_PicksAConnectedWorkspaceAtRandom()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            var routedPaths = new HashSet<string>(StringComparer.Ordinal);

            for (var i = 0; i < 20; i++)
            {
                using var response = await this.PostChatAsync("/v1/chat/completions");
                Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            }

            // Every request went to exactly one of the connected workspaces, with its own token.
            var recorded = this.upstream.Requests;
            Assert.Equal(20, recorded.Count);
            Assert.All(recorded, request => Assert.Contains(request.Authorization, new[] { "Bearer cnb-token-A", "Bearer cnb-token-B" }));
            Assert.All(recorded, request => Assert.Contains(request.Path, new[] { "/orgA/repoA/-/ai/chat/completions", "/orgB/repoB/-/ai/chat/completions" }));
            Assert.All(recorded, request => routedPaths.Add(request.Path));

            // 20 coin flips: the chance of one workspace never being picked is ~2e-6.
            Assert.Equal(2, routedPaths.Count);
        }

        [Fact]
        public async Task BareChat_WithoutAnyConnectedWorkspace_IsBadGateway()
        {
            using var response = await this.PostChatAsync("/v1/chat/completions");

            Assert.Equal(HttpStatusCode.BadGateway, response.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task UnsupportedPrefixShapes_AreNotFound()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var singleSegment = await this.GetAsync("/org~repoA/v1/models");
            using var threeSegments = await this.GetAsync("/a/b/c/v1/models");
            using var otherEndpoint = await this.PostChatAsync("/orgA/repoA/v1/embeddings");

            Assert.Equal(HttpStatusCode.NotFound, singleSegment.StatusCode);
            Assert.Equal(HttpStatusCode.NotFound, threeSegments.StatusCode);
            Assert.Equal(HttpStatusCode.NotFound, otherEndpoint.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task WrongMethod_IsMethodNotAllowed() 
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var postToModels = await this.PostChatAsync("/orgA/repoA/v1/models");

            Assert.Equal(HttpStatusCode.MethodNotAllowed, postToModels.StatusCode);
            Assert.Empty(this.upstream.Requests);
        }

        [Fact]
        public async Task Chat_RequiresTheApiKey()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            using var ok = await this.PostChatAsync("/orgA/repoA/v1/chat/completions");
            using var wrong = await this.PostChatAsync("/orgA/repoA/v1/chat/completions", "pk-wrong");
            using var missing = await this.PostChatAsync("/orgA/repoA/v1/chat/completions", apiKey: null);

            Assert.Equal(HttpStatusCode.OK, ok.StatusCode);
            Assert.Equal(HttpStatusCode.Unauthorized, wrong.StatusCode);
            Assert.Equal(HttpStatusCode.Unauthorized, missing.StatusCode);

            // Only the request with the right key reached the upstream.
            Assert.Single(this.upstream.Requests);
        }

        [Fact]
        public async Task Models_IsOpenSoTheKeepaliveProbeNeedsNoKey()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            // No Authorization header at all, exactly like the keepalive cron's probe.
            using var anonymous = await this.publicClient.GetAsync("/orgA/repoA/v1/models");
            using var wrongKey = await this.GetAsync("/orgA/repoA/v1/models", "pk-wrong");

            Assert.Equal(HttpStatusCode.OK, anonymous.StatusCode);
            Assert.Equal(HttpStatusCode.OK, wrongKey.StatusCode);
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
            request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {ApiKey}");

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

        private Task<HttpResponseMessage> PostChatAsync(string path, string? apiKey = ApiKey)
        {
            var request = new HttpRequestMessage(HttpMethod.Post, path)
            {
                Content = new StringContent(ChatBody, Encoding.UTF8, "application/json"),
            };

            if (apiKey != null)
            {
                request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {apiKey}");
            }

            return this.publicClient.SendAsync(request, HttpCompletionOption.ResponseHeadersRead);
        }

        private Task<HttpResponseMessage> GetAsync(string path, string? apiKey = ApiKey)
        {
            var request = new HttpRequestMessage(HttpMethod.Get, path);
            if (apiKey != null)
            {
                request.Headers.TryAddWithoutValidation("Authorization", $"Bearer {apiKey}");
            }

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
