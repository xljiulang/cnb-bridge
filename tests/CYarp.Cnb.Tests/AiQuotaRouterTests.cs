using CYarpServer;
using CYarpServer.Quota;
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
        /// The routing floor these tests pass explicitly, so a change of the configured default
        /// cannot silently rewrite what the cases assert. One credit, like the shipped default.
        /// </summary>
        private const long Floor = 1_000L;

        /// <summary>
        /// Records a reading where the organization has spent nothing yet, so the allowance equals
        /// the remainder. Routing only looks at the remainder, so the total is irrelevant here.
        /// </summary>
        private static void Report(AiQuotaStore store, string clientId, long remainingMilliCredits, DateTimeOffset now)
        {
            store.ReportSuccess(clientId, totalMilliCredits: remainingMilliCredits, remainingMilliCredits, now);
        }

        /// <summary>
        /// Runs the router over the given candidates for many seeds and returns the distinct ids it
        /// ever picked, together with the tier of the first run. Every call passes
        /// <paramref name="floor"/> explicitly, so nothing here depends on the shipped default.
        /// </summary>
        private static (HashSet<string> Picked, AiQuotaTier Tier, long? Remaining) PickMany(
            IReadOnlyList<CYarp.Server.IClient> clients,
            AiQuotaStore store,
            long floor)
        {
            var picked = new HashSet<string>(StringComparer.Ordinal);
            var tier = AiQuotaTier.Disabled;
            long? remaining = null;

            for (var seed = 0; seed < 40; seed++)
            {
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: true, Now, StaleAfter, floor, new Random(seed));
                Assert.NotNull(selection);
                picked.Add(selection!.Value.Client.Id);

                if (seed == 0)
                {
                    tier = selection.Value.Tier;
                    remaining = selection.Value.RemainingMilliCredits;
                }
            }

            return (picked, tier, remaining);
        }

        [Fact]
        public void NoCandidates_ReturnsNull()
        {
            var selection = AiQuotaRouter.Pick([], new AiQuotaStore(), quotaRoutingEnabled: true, Now, StaleAfter, Floor, new Random(1));

            Assert.Null(selection);
        }

        [Fact]
        public void AboveTheFloor_SharesTheTrafficRegardlessOfTheAmount()
        {
            // 100 credits versus 5 credits: both clear the floor, so neither one owns the traffic.
            // This is what replaced "the biggest remaining quota wins".
            var store = new AiQuotaStore();
            Report(store, "orgB/repoB", 100_000L, Now);
            Report(store, "orgA/repoA", 5_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, remaining) = PickMany(clients, store, Floor);

            Assert.Equal(2, picked.Count);
            Assert.Equal(AiQuotaTier.Available, tier);
            Assert.NotNull(remaining);
        }

        [Fact]
        public void AtTheFloor_LosesToAnAboveTheFloorWorkspace()
        {
            // The comparison is strict: 1 credit is exactly the default floor, so it is not eligible.
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 1_000L, Now);
            Report(store, "orgB/repoB", 100_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, remaining) = PickMany(clients, store, Floor);

            Assert.Equal("orgB/repoB", Assert.Single(picked));
            Assert.Equal(AiQuotaTier.Available, tier);
            Assert.Equal(100_000L, remaining);
        }

        [Fact]
        public void BelowTheFloor_FallsIntoTheInsufficientTier()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 200L, Now);
            Report(store, "orgB/repoB", 1_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, _) = PickMany(clients, store, Floor);

            // Both are known and both are below the floor, so they still share the traffic as a
            // fallback rather than failing the request.
            Assert.Equal(2, picked.Count);
            Assert.Equal(AiQuotaTier.Insufficient, tier);
        }

        [Fact]
        public void AFloorOfZero_MakesAnyPositiveQuotaEligible()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 1L, Now);
            Report(store, "orgB/repoB", 100_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, _) = PickMany(clients, store, floor: 0L);

            // A single milli-credit is above a zero floor, so the old "any positive quota" rule
            // is still reachable through configuration.
            Assert.Equal(2, picked.Count);
            Assert.Equal(AiQuotaTier.Available, tier);
        }

        [Fact]
        public void Unknown_DoesNotBeatAnAboveTheFloorQuota()
        {
            var store = new AiQuotaStore();
            Report(store, "orgB/repoB", 2_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, remaining) = PickMany(clients, store, Floor);

            // Anything above the floor still beats a workspace whose quota is unknown.
            Assert.Equal("orgB/repoB", Assert.Single(picked));
            Assert.Equal(AiQuotaTier.Available, tier);
            Assert.Equal(2_000L, remaining);
        }

        [Fact]
        public void Unknown_BeatsABelowTheFloorWorkspace()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 500L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, remaining) = PickMany(clients, store, Floor);

            // An unknown workspace may still have plenty, while this one is known to be short.
            Assert.Equal("orgB/repoB", Assert.Single(picked));
            Assert.Equal(AiQuotaTier.Unknown, tier);
            Assert.Null(remaining);
        }

        [Fact]
        public void StaleQuota_IsTreatedAsUnknown()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 1_000_000L, Now - StaleAfter - TimeSpan.FromSeconds(1d));
            Report(store, "orgB/repoB", 5_000L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, remaining) = PickMany(clients, store, Floor);

            // The bigger value is stale, so it must not win over the fresh smaller one.
            Assert.Equal("orgB/repoB", Assert.Single(picked));
            Assert.Equal(AiQuotaTier.Available, tier);
            Assert.Equal(5_000L, remaining);
        }

        [Fact]
        public void AllInsufficient_StillPicksOne()
        {
            var store = new AiQuotaStore();
            Report(store, "orgA/repoA", 0L, Now);
            Report(store, "orgB/repoB", -10L, Now);
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, _) = PickMany(clients, store, Floor);

            Assert.Equal(2, picked.Count);
            Assert.Equal(AiQuotaTier.Insufficient, tier);
        }

        [Fact]
        public void AllUnknown_PicksUniformly()
        {
            var clients = Candidates("orgA/repoA", "orgB/repoB");

            var (picked, tier, _) = PickMany(clients, new AiQuotaStore(), Floor);

            Assert.Equal(2, picked.Count);
            Assert.Equal(AiQuotaTier.Unknown, tier);
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
                var selection = AiQuotaRouter.Pick(clients, store, quotaRoutingEnabled: false, Now, StaleAfter, Floor, new Random(seed));

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
