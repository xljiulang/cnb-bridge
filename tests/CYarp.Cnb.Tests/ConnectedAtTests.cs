using CYarpServer;
using CYarpServer.Forwarding;
using CYarpServer.Options;
using CYarpServer.Quota;
using System;
using System.Linq;
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

        [Fact]
        public void ProjectClients_SortsByRemainingCreditsDescending()
        {
            var now = Now;
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", remainingMilliCredits: 1_000L, now);
            Report(store, "orgB/repoB", remainingMilliCredits: 500_000L, now);

            // A negative remainder is a real state - the organization has overspent - and it has to
            // land after every positive one rather than being treated as an error.
            Report(store, "orgC/repoC", remainingMilliCredits: -20_000L, now);
            Report(store, "orgD/repoD", remainingMilliCredits: 0L, now);

            var clients = HttpForwardHandler.ProjectClients(
                [new FakeClient("orgC/repoC"), new FakeClient("orgA/repoA"), new FakeClient("orgD/repoD"), new FakeClient("orgB/repoB")],
                store,
                new AiQuotaOptions(),
                now);

            Assert.Equal(["orgB/repoB", "orgA/repoA", "orgD/repoD", "orgC/repoC"], clients.Select(client => client.ClientId));
        }

        [Fact]
        public void ProjectClients_PutsUnknownRemaindersLast()
        {
            var now = Now;
            var store = new AiQuotaStore();
            Report(store, "orgB/repoB", remainingMilliCredits: 3_000L, now);

            // A probe that failed before ever succeeding leaves an error and no value at all.
            store.ReportFailure("orgC/repoC", "401 Unauthorized");

            // orgA was never probed; orgC was probed but produced no reading. Neither has a number,
            // so both belong after the one workspace that does - even though its value is small.
            var clients = HttpForwardHandler.ProjectClients(
                [new FakeClient("orgA/repoA"), new FakeClient("orgC/repoC"), new FakeClient("orgB/repoB")],
                store,
                new AiQuotaOptions(),
                now);

            Assert.Equal(["orgB/repoB", "orgA/repoA", "orgC/repoC"], clients.Select(client => client.ClientId));
            Assert.Null(clients[1].RemainingCredits);
            Assert.Null(clients[2].RemainingCredits);
            Assert.Equal("401 Unauthorized", clients[2].Error);
        }

        [Fact]
        public void ProjectClients_BreaksTiesByClientId()
        {
            var now = Now;
            var store = new AiQuotaStore();

            // Workspaces of one organization always report the same allowance, so ties are the
            // normal case rather than an edge one - and "every value unknown" is a tie of nulls.
            Report(store, "orgZ/repoZ", remainingMilliCredits: 7_000L, now);
            Report(store, "orgA/repoA", remainingMilliCredits: 7_000L, now);

            var clients = HttpForwardHandler.ProjectClients(
                [new FakeClient("orgZ/repoZ"), new FakeClient("orgM/repoM"), new FakeClient("orgA/repoA")],
                store,
                new AiQuotaOptions(),
                now);

            // The two tied values sort by id; the unknown one is last even though its id is lowest.
            Assert.Equal(["orgA/repoA", "orgZ/repoZ", "orgM/repoM"], clients.Select(client => client.ClientId));
        }

        [Fact]
        public void ProjectClients_KeepsAStaleReadingInNumericOrder()
        {
            var now = Now;
            var options = new AiQuotaOptions();
            var store = new AiQuotaStore();

            // Read well before the staleness threshold: still a number, so it sorts by that number.
            var staleMoment = now - options.StaleAfter - TimeSpan.FromMinutes(1d);
            Report(store, "orgA/repoA", remainingMilliCredits: 900_000L, staleMoment);
            Report(store, "orgB/repoB", remainingMilliCredits: 5_000L, now);

            var clients = HttpForwardHandler.ProjectClients(
                [new FakeClient("orgA/repoA"), new FakeClient("orgB/repoB")],
                store,
                options,
                now);

            // Stale only changes what routing does, not where the row sits in this list.
            Assert.Equal(["orgA/repoA", "orgB/repoB"], clients.Select(client => client.ClientId));
            Assert.True(clients[0].Stale);
            Assert.False(clients[1].Stale);
        }

        [Fact]
        public void ProjectClients_OfNothing_ReturnsAnEmptyArray()
        {
            var clients = HttpForwardHandler.ProjectClients([], new AiQuotaStore(), new AiQuotaOptions(), Now);

            Assert.Empty(clients);
        }

        /// <summary>Now, a fixed instant so the staleness verdicts in these tests are exact.</summary>
        private static readonly DateTimeOffset Now = new(2026, 9, 23, 8, 0, 0, TimeSpan.Zero);

        /// <summary>
        /// Records a reading whose allowance equals the remainder, i.e. nothing spent. Only the
        /// remainder matters for ordering, so the total stays out of the way.
        /// </summary>
        private static void Report(AiQuotaStore store, string clientId, long remainingMilliCredits, DateTimeOffset now)
        {
            store.ReportSuccess(clientId, totalMilliCredits: remainingMilliCredits, remainingMilliCredits, now);
        }
    }
}
