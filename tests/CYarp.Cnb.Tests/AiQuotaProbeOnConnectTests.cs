using System;
using System.Net;
using System.Threading.Tasks;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// A workspace must have its quota read as soon as it connects, rather than only on the next
    /// periodic round.
    /// <para>
    /// The probe period here is deliberately far longer than the test is willing to wait (10 minutes
    /// against a 3 second assertion), so the periodic loop cannot possibly be what satisfies the
    /// assertion: only the connect event can. Without that path this test would have to wait ten
    /// minutes and would fail.
    /// </para>
    /// </summary>
    public sealed class AiQuotaProbeOnConnectTests : AiQuotaTestBench
    {
        /// <inheritdoc/>
        protected override string ProbeInterval => "00:10:00";

        [Fact]
        public async Task Quota_IsReadImmediatelyWhenAWorkspaceConnects()
        {
            this.Upstream.SetQuota("orgA", totalMilliCredits: 100_000L, usedMilliCredits: 25_000L);

            await this.ConnectAsync("orgA/repoA", "/orgA/repoA/-/ai", "cnb-token-A");

            // Three seconds is orders of magnitude below the 10 minute period, and comfortably above
            // the time one probe over a local tunnel needs.
            var snapshot = await this.WaitForQuotaAsync("orgA/repoA", timeout: TimeSpan.FromSeconds(3d));

            Assert.Equal(100_000L, snapshot.TotalMilliCredits);
            Assert.Equal(75_000L, snapshot.RemainingMilliCredits);

            // And the listing shows it right away, which is what a caller actually observes.
            using var response = await this.GetAsync("/", apiKey: null);
            Assert.Equal(HttpStatusCode.OK, response.StatusCode);
            Assert.Contains("\"remainingCredits\":75", await response.Content.ReadAsStringAsync(), StringComparison.Ordinal);
        }
    }
}
