using CYarpServer;
using CYarpServer.Forwarding;
using CYarpServer.Options;
using CYarpServer.Quota;
using System;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Unit tests of the time fields of <c>GET /</c> (<c>connectedAt</c> and <c>updatedAt</c>).
    /// <para>
    /// These exist because the HTTP level tests cannot catch the interesting mistake: this build
    /// machine is UTC+8, where relabelling a timestamp is indistinguishable from converting it, so
    /// a wrong implementation passes there and only breaks on a UTC host. Feeding a known UTC
    /// instant in directly makes the assertion independent of the machine's zone.
    /// </para>
    /// </summary>
    public sealed class ConnectedAtTests
    {
        [Fact]
        public void ToResponseTime_ConvertsTheInstant_NotJustTheOffset()
        {
            // 03:29Z is 11:29 in +08:00. Relabelling the offset would leave the wall clock at 03:29,
            // which is 8 hours away from the same instant - exactly the bug this guards against.
            var time = new DateTimeOffset(2026, 9, 23, 3, 29, 0, TimeSpan.Zero);

            var rendered = HttpForwardHandler.ToResponseTime(time);

            Assert.Equal(TimeSpan.FromHours(8d), rendered.Offset);
            Assert.Equal(new DateTimeOffset(2026, 9, 23, 11, 29, 0, TimeSpan.FromHours(8d)), rendered);

            // Same instant: a caller comparing instants must see no shift at all.
            Assert.Equal(time, rendered);
            Assert.Equal(time.UtcDateTime, rendered.UtcDateTime);
        }

        [Fact]
        public void ToResponseTime_IsIndependentOfTheOffsetItWasGiven()
        {
            // The host's zone decides the offset CYarp stamps on CreationTime; the rendered value
            // must not, otherwise the field would read differently per deployment.
            var instant = new DateTimeOffset(2026, 9, 23, 3, 29, 0, TimeSpan.Zero);
            var asUtcHost = instant;
            var asLocalHost = instant.ToOffset(TimeSpan.FromHours(8d));
            var asOtherHost = instant.ToOffset(TimeSpan.FromHours(-5d));

            var expected = HttpForwardHandler.ToResponseTime(asUtcHost);

            Assert.Equal(expected, HttpForwardHandler.ToResponseTime(asLocalHost));
            Assert.Equal(expected, HttpForwardHandler.ToResponseTime(asOtherHost));
        }

        [Fact]
        public void ClientInfo_RendersBothTimestampsInTheSameFixedOffset()
        {
            // An explicit "now" makes the staleness relation between the two timestamps obvious:
            // the tunnel was established first, the quota was read a minute ago (so still fresh).
            var now = new DateTimeOffset(2026, 3, 4, 6, 7, 8, TimeSpan.Zero);
            var creationTime = now - TimeSpan.FromMinutes(30d);
            var updatedAt = now - TimeSpan.FromMinutes(1d);
            var client = new FakeClient("orgA/repoA", creationTime: creationTime);
            var store = new AiQuotaStore();
            store.ReportSuccess("orgA/repoA", totalMilliCredits: 500_000L, remainingMilliCredits: 12_345L, updatedAt);

            var info = HttpForwardHandler.ToClientInfo(client, store, new AiQuotaOptions(), now);

            Assert.Equal("orgA/repoA", info.ClientId);
            Assert.Equal(new DateTimeOffset(2026, 3, 4, 13, 37, 8, TimeSpan.FromHours(8d)), info.ConnectedAt);

            // One payload, one offset: a caller must not have to reconcile two of them.
            var renderedUpdatedAt = Assert.NotNull(info.UpdatedAt);
            Assert.Equal(TimeSpan.FromHours(8d), renderedUpdatedAt.Offset);
            Assert.Equal(new DateTimeOffset(2026, 3, 4, 14, 6, 8, TimeSpan.FromHours(8d)), renderedUpdatedAt);
            Assert.Equal(info.ConnectedAt.Offset, renderedUpdatedAt.Offset);

            // Converting the offset must not disturb the value: the instant and the freshness that
            // the routing relies on are untouched.
            Assert.Equal(updatedAt, renderedUpdatedAt);
            Assert.True(renderedUpdatedAt > info.ConnectedAt);
            Assert.Equal(12.345m, info.RemainingCredits);
            Assert.Equal(500m, info.TotalCredits);
            Assert.False(info.Stale);
            Assert.Null(info.Error);
        }

        [Fact]
        public void ClientInfo_WithoutAnyProbe_ReportsAnUnknownQuotaAndNoUpdatedAt()
        {
            var client = new FakeClient("orgA/repoA");

            var info = HttpForwardHandler.ToClientInfo(client, new AiQuotaStore(), new AiQuotaOptions(), DateTimeOffset.UtcNow);

            Assert.Null(info.RemainingCredits);
            Assert.True(info.Stale);
            Assert.Null(info.UpdatedAt);
            Assert.Null(info.Error);
        }
    }
}
