using System;
using System.Net;
using System.Threading.Tasks;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// With <c>AiQuota:Enabled=false</c> the server behaves as it did before the quota feature:
    /// no probe, an unknown quota in <c>GET /</c>, and a uniform pick on the bare chat route.
    /// </summary>
    public sealed class AiQuotaDisabledTests : AiQuotaTestBench
    {
        /// <inheritdoc/>
        protected override bool QuotaRoutingEnabled => false;

        [Fact]
        public async Task QuotaStaysUnknown_AndTheBareChatRouteIsUniform()
        {
            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");
            await this.ConnectAsync("orgB/repoB", "/orgB/repoB/-/ai", "cnb-token-B");

            // Connecting now raises a probe signal, so this also covers "disabled means no probe on
            // connect": the events are delivered, yet nothing is read. Give them ample time to leave
            // a trace if they were ever acted upon.
            await Task.Delay(2_000);
            Assert.False(this.Store.TryGet("orgA/repoA", out _));
            Assert.False(this.Store.TryGet("orgB/repoB", out _));

            using (var list = await this.GetAsync("/", apiKey: null))
            {
                Assert.Equal(HttpStatusCode.OK, list.StatusCode);
                var body = await list.Content.ReadAsStringAsync();
                Assert.Contains("\"remainingCredits\":null", body, StringComparison.Ordinal);
                Assert.Contains("\"stale\":true", body, StringComparison.Ordinal);
            }

            // Nothing is known about any workspace, so the pick is uniform again: both workspaces
            // must show up in the forwarded traffic.
            for (var i = 0; i < 20; i++)
            {
                using var response = await this.PostChatAsync("/v1/chat/completions");
                Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            }

            var recorded = this.Upstream.Requests;
            Assert.Equal(20, recorded.Count);
            Assert.Contains(recorded, request => request.Path == "/orgA/repoA/-/ai/chat/completions");
            Assert.Contains(recorded, request => request.Path == "/orgB/repoB/-/ai/chat/completions");
        }
    }
}
