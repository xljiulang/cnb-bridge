using CYarpServer;
using System;
using System.Collections.Generic;
using System.Linq;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Unit tests of the quota based routing decision. The clock and the random source are
    /// injected, so every case is exact rather than statistical.
    /// </summary>
    public sealed class AiQuotaRouterTests
    {
        private static readonly DateTimeOffset Now = new(2026, 9, 23, 8, 0, 0, TimeSpan.Zero);
        private static readonly TimeSpan StaleAfter = TimeSpan.FromMinutes(6d);

        /// <summary>
        /// Records a reading where the organization has spent nothing yet, so the allowance equals
        /// the remainder. Routing only looks at the remainder, so the total is irrelevant here.
        /// </summary>
        private static void Report(AiQuotaStore store, string clientId, long remainingMilliCredits, DateTimeOffset now)
        {
            store.ReportSuccess(clientId, totalMilliCredits: remainingMilliCredits, remainingMilliCredits, now);
        }

        [Fact]
        public void NoCandidates_ReturnsNull()
        {
            var selection = AiQuotaRouter.Pick([], new AiQuotaStore(), quotaRoutingEnabled: true, Now, StaleAfter, new Random(1));

            Assert.Null(selection);
        }

        [Fact]
        public void KnownQuota_PicksTheHighestRemaining()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 1_000L, Now);
            Report(store, "orgB/repoB", 100_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, new Random(7));

            Assert.NotNull(selection);
            Assert.Equal("orgB/repoB", selection!.Value.Client.Id);
            Assert.Equal(AiQuotaTier.Known, selection.Value.Tier);
            Assert.Equal(100_000L, selection.Value.RemainingMilliCredits);
        }

        [Fact]
        public void TiedQuota_SharesTheTraffic()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 5_000L, Now);
            Report(store, "orgB/repoB", 5_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var picked = new HashSet<string>(StringComparer.Ordinal);
            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, new Random(seed));
                Assert.Equal(AiQuotaTier.Known, selection!.Value.Tier);
                picked.Add(selection.Value.Client.Id);
            }

            // Both candidates must be reachable: a tie is a shared maximum, not a fixed winner.
            Assert.Equal(2, picked.Count);
        }

        [Fact]
        public void Unknown_DoesNotBeatAKnownQuota()
        {
            var store = new AiQuotaStore();
            Report(store, "orgB/repoB", 1L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, new Random(seed));

                // One single credit is still better than an unknown workspace.
                Assert.Equal("orgB/repoB", selection!.Value.Client.Id);
                Assert.Equal(AiQuotaTier.Known, selection.Value.Tier);
            }
        }

        [Fact]
        public void Unknown_BeatsAnExhaustedWorkspace()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 0L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, new Random(seed));

                Assert.Equal("orgB/repoB", selection!.Value.Client.Id);
                Assert.Equal(AiQuotaTier.Unknown, selection.Value.Tier);
                Assert.Null(selection.Value.RemainingMilliCredits);
            }
        }

        [Fact]
        public void StaleQuota_IsTreatedAsUnknown()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 1_000_000L, Now - StaleAfter - TimeSpan.FromSeconds(1d));
            Report(store, "orgB/repoB", 2_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, new Random(3));

            // The bigger value is stale, so it must not win over the fresh smaller one.
            Assert.Equal("orgB/repoB", selection!.Value.Client.Id);
            Assert.Equal(AiQuotaTier.Known, selection.Value.Tier);
            Assert.Equal(2_000L, selection.Value.RemainingMilliCredits);
        }

        [Fact]
        public void AllExhausted_StillPicksOne()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 0L, Now);
            Report(store, "orgB/repoB", -10L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var picked = new HashSet<string>(StringComparer.Ordinal);
            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, new Random(seed));

                Assert.Equal(AiQuotaTier.Exhausted, selection!.Value.Tier);
                picked.Add(selection.Value.Client.Id);
            }

            Assert.Equal(2, picked.Count);
        }

        [Fact]
        public void AllUnknown_PicksUniformly()
        {
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var picked = new HashSet<string>(StringComparer.Ordinal);
            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, new AiQuotaStore(), quotaRoutingEnabled: true, Now, StaleAfter, new Random(seed));

                Assert.Equal(AiQuotaTier.Unknown, selection!.Value.Tier);
                picked.Add(selection.Value.Client.Id);
            }

            Assert.Equal(2, picked.Count);
        }

        [Fact]
        public void Disabled_IgnoresTheProbedQuota()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 0L, Now);
            Report(store, "orgB/repoB", 1_000_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var picked = new HashSet<string>(StringComparer.Ordinal);
            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: false, Now, StaleAfter, new Random(seed));

                Assert.Equal(AiQuotaTier.Disabled, selection!.Value.Tier);
                Assert.Null(selection.Value.RemainingMilliCredits);
                picked.Add(selection.Value.Client.Id);
            }

            // Off means the routing is uniformly random again, exhausted workspace included.
            Assert.Equal(2, picked.Count);
        }

        [Fact]
        public void AFailedProbeKeepsTheValueButMakesItUnknown()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 500L, Now);

            // A failure records the reason and keeps the last value; the timestamp is untouched,
            // so the reading ages out on its own instead of being refreshed for free.
            store.ReportFailure("orgA/repoA", "401 Unauthorized");

            Assert.True(store.TryGet("orgA/repoA", out var snapshot));
            Assert.Equal(500L, snapshot.RemainingMilliCredits);
            Assert.Equal(Now, snapshot.UpdatedAt);
            Assert.Equal("401 Unauthorized", snapshot.Error);

            snapshot = snapshot with { UpdatedAt = Now - StaleAfter - TimeSpan.FromSeconds(1d) };
            Assert.False(snapshot.TryGetRemaining(Now, StaleAfter, out _));
        }

        private static IReadOnlyList<CYarp.Server.IClient> Candidates(params string[] clientIds)
        {
            return clientIds.Select(id => (CYarp.Server.IClient)new FakeClient(id)).ToArray();
        }
    }
}
