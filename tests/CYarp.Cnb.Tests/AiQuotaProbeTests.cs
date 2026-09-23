using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;
using System;
using System.Linq;
using System.Net;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;
using Xunit;
using CYarp.Server;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// The server reads the organization quota through the tunnel of each workspace, reports it on
    /// <c>GET /</c>, and the bare chat route prefers the workspace with the most remaining credits.
    /// </summary>
    public sealed class AiQuotaProbeTests : AiQuotaTestBench
    {
        [Fact]
        public async Task Quota_IsReadThroughTheWorkspaceTunnel()
        {
            this.Upstream.SetQuota("orgA", totalMilliCredits: 100_000L, usedMilliCredits: 25_000L, frozenMilliCredits: 500L);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            var snapshot = await this.WaitForQuotaAsync("orgA/repoA");

            // The allowance of the month, and what is left of it after the spent and frozen parts.
            Assert.Equal(100_000L, snapshot.TotalMilliCredits);
            Assert.Equal(74_500L, snapshot.RemainingMilliCredits);
            Assert.Null(snapshot.Error);

            // The probe must go through the tunnel of this workspace, with its own CNB token: the
            // charge endpoints only answer requests that originate inside CNB.
            var chargeRequests = this.Upstream.ChargeRequests;
            Assert.Contains(chargeRequests, request => request.Path == "/orgA/-/charge/quota" && request.Authorization == "Bearer cnb-token-A");
            Assert.Contains(chargeRequests, request => request.Path == "/orgA/-/charge/volume" && request.Authorization == "Bearer cnb-token-A");
        }

        [Fact]
        public async Task BareChat_PrefersTheWorkspaceWithMoreCredits()
        {
            this.Upstream.SetQuota("orgA", totalMilliCredits: 100_000L, usedMilliCredits: 0L);
            this.Upstream.SetQuota("orgB", totalMilliCredits: 1_000L, usedMilliCredits: 0L);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");
            await this.WaitForQuotaAsync("orgA/repoA");
            await this.WaitForQuotaAsync("orgB/repoB");

            for (var i = 0; i < 10; i++)
            {
                using var response = await this.PostChatAsync("/v1/chat/completions");
                Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            }

            var recorded = this.Upstream.Requests;
            Assert.Equal(10, recorded.Count);
            Assert.All(recorded, request => Assert.Equal("/orgA/repoA/-/ai/chat/completions", request.Path));
            Assert.All(recorded, request => Assert.Equal("Bearer cnb-token-A", request.Authorization));
        }

        [Fact]
        public async Task BareChat_AvoidsAnExhaustedWorkspace()
        {
            // orgA is out of credits and orgB still has some: a successfully read quota of zero
            // must not attract traffic the way an unknown one does.
            this.Upstream.SetQuota("orgA", totalMilliCredits: 10_000L, usedMilliCredits: 10_000L);
            this.Upstream.SetQuota("orgB", totalMilliCredits: 5_000L, usedMilliCredits: 3_000L);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            var exhausted = await this.WaitForQuotaAsync("orgA/repoA");
            Assert.Equal(10_000L, exhausted.TotalMilliCredits);
            Assert.Equal(0L, exhausted.RemainingMilliCredits);
            await this.WaitForQuotaAsync("orgB/repoB");

            for (var i = 0; i < 5; i++)
            {
                using var response = await this.PostChatAsync("/v1/chat/completions");
                Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            }

            Assert.All(this.Upstream.Requests, request => Assert.Equal("/orgB/repoB/-/ai/chat/completions", request.Path));
        }

        [Fact]
        public async Task Quota_IsProbedOverATcpTunnelOfTheWorkspace()
        {
            // The probe must open a tunnel instead of calling the charge endpoint directly: a
            // direct call from the server is rejected by CNB's network policy in production, and
            // the tunnel is also where the workspace's own credential comes from. Holding the
            // charge response open leaves the tunnel in place, which is what this asserts on.
            this.Upstream.SetQuota("orgA", totalMilliCredits: 50_000L, usedMilliCredits: 0L);
            this.Upstream.HoldCharge();

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            var viewer = this.Server.Services.GetRequiredService<IClientViewer>();
            var deadline = DateTime.UtcNow.AddSeconds(30d);
            var tunnelCount = 0;

            while (DateTime.UtcNow < deadline && tunnelCount == 0)
            {
                if (viewer.TryGetValue("orgA/repoA", out var client))
                {
                    tunnelCount = client.TcpTunnelCount;
                }

                if (tunnelCount == 0)
                {
                    await Task.Delay(50);
                }
            }

            this.Upstream.ReleaseCharge();

            // A held quota response means a tunnel of this workspace is open right now.
            Assert.True(tunnelCount > 0, "the quota probe should hold a TCP tunnel of the workspace while its request is in flight");

            var snapshot = await this.WaitForQuotaAsync("orgA/repoA");
            Assert.Equal(50_000L, snapshot.RemainingMilliCredits);
        }

        [Fact]
        public async Task ChargeFailure_IsReportedAndDoesNotBreakRouting()
        {
            // The CNB charge endpoints need the group-resource:r permission, which the workspace
            // token may well lack: the probe then records the reason and routing keeps working.
            this.Upstream.SetChargeFailure(StatusCodes.Status403Forbidden);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            await this.WaitForQuotaErrorAsync("orgA/repoA");

            using (var list = await this.GetAsync("/", apiKey: null))
            {
                Assert.Equal(HttpStatusCode.OK, list.StatusCode);

                using var payload = JsonDocument.Parse(await list.Content.ReadAsStringAsync());
                foreach (var element in payload.RootElement.EnumerateArray())
                {
                    Assert.Equal(JsonValueKind.Null, element.GetProperty("totalCredits").ValueKind);
                    Assert.Equal(JsonValueKind.Null, element.GetProperty("remainingCredits").ValueKind);
                    Assert.True(element.GetProperty("stale").GetBoolean());
                    Assert.Equal(JsonValueKind.Null, element.GetProperty("updatedAt").ValueKind);
                    Assert.Contains("403", element.GetProperty("error").GetString(), StringComparison.Ordinal);
                }
            }

            for (var i = 0; i < 5; i++)
            {
                using var response = await this.PostChatAsync("/v1/chat/completions");
                Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            }
        }

        [Fact]
        public async Task ClientList_ReportsTheProbedQuota()
        {
            // Distinct numbers on purpose: the allowance (200) and the remainder (100.5) must not be
            // the same value, or the test could not tell which field was wired to which reading.
            this.Upstream.SetQuota("orgA", totalMilliCredits: 200_000L, usedMilliCredits: 99_500L);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.WaitForQuotaAsync("orgA/repoA");

            // The list is open, exactly as it was before this feature.
            using var response = await this.GetAsync("/", apiKey: null);
            using var wrongKey = await this.GetAsync("/", "pk-wrong");

            Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            Assert.Equal(HttpStatusCode.OK, wrongKey.StatusCode);

            using var payload = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
            var element = Assert.Single(payload.RootElement.EnumerateArray().ToArray());

            // Every object carries the same keys, the null ones included.
            Assert.Equal("orgA/repoA", element.GetProperty("clientId").GetString());
            Assert.Equal(200m, element.GetProperty("totalCredits").GetDecimal());
            Assert.Equal(100.5m, element.GetProperty("remainingCredits").GetDecimal());
            Assert.False(element.GetProperty("stale").GetBoolean());
            Assert.Equal(JsonValueKind.Null, element.GetProperty("error").ValueKind);

            // All timestamps of the payload use the same fixed offset.
            var updatedAt = element.GetProperty("updatedAt").GetDateTimeOffset();
            Assert.Equal(TimeSpan.FromHours(8d), updatedAt.Offset);
            Assert.True(
                (DateTimeOffset.UtcNow - updatedAt).Duration() < TimeSpan.FromMinutes(1d),
                $"updatedAt {updatedAt:O} should be when this test's quota was just read");

            // connectedAt is the moment this tunnel was established, in a fixed +08:00 offset, so
            // the value must be the present rather than any constant. The tunnel was opened moments
            // ago in this test, hence the narrow window.
            var connectedAt = element.GetProperty("connectedAt").GetDateTimeOffset();
            Assert.Equal(TimeSpan.FromHours(8d), connectedAt.Offset);
            Assert.Equal(updatedAt.Offset, connectedAt.Offset);
            Assert.True(
                (DateTimeOffset.UtcNow - connectedAt).Duration() < TimeSpan.FromMinutes(1d),
                $"connectedAt {connectedAt:O} should be the time this test's tunnel was established");
        }
    }
}
