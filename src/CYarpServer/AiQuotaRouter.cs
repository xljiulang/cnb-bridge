using CYarp.Server;
using System;
using System.Collections.Generic;

namespace CYarpServer
{
    /// <summary>
    /// Picks the workspace of a bare (unified base_url) chat request from the quota the server
    /// has probed: the workspace with the most remaining credits wins, and only when no usable
    /// quota is known does the choice fall back to a uniform pick among the unknown ones.
    /// </summary>
    public static class AiQuotaRouter
    {
        /// <summary>
        /// Picks a workspace, or null when there is no candidate at all.
        /// <para>
        /// Candidates are grouped into three tiers: <see cref="AiQuotaTier.Known"/> (fresh quota
        /// above zero, the highest value wins), <see cref="AiQuotaTier.Unknown"/> (never probed
        /// or too old) and <see cref="AiQuotaTier.Exhausted"/> (fresh quota at or below zero).
        /// The first non empty tier decides, and within a tier the pick is uniform - so
        /// workspaces that tie, and the fallback tiers, still share the traffic. With quota
        /// routing switched off every candidate is one uniform tier, which reproduces the
        /// behaviour that existed before this feature.
        /// </para>
        /// </summary>
        /// <param name="clients">The connected workspaces (a snapshot; the viewer mutates concurrently).</param>
        /// <param name="store">The probed quota.</param>
        /// <param name="quotaRoutingEnabled">Whether quota routing is switched on.</param>
        /// <param name="now">Current time.</param>
        /// <param name="staleAfter">Maximum age of a usable snapshot.</param>
        /// <param name="random">The random source, so tests can seed it.</param>
        /// <returns>The selection, or null when <paramref name="clients"/> is empty.</returns>
        public static AiQuotaSelection? Pick(
            IReadOnlyList<IClient> clients,
            AiQuotaStore store,
            bool quotaRoutingEnabled,
            DateTimeOffset now,
            TimeSpan staleAfter,
            Random random)
        {
            if (clients.Count == 0)
            {
                return null;
            }

            if (quotaRoutingEnabled == false)
            {
                return new AiQuotaSelection(clients[random.Next(clients.Count)], AiQuotaTier.Disabled, null);
            }

            // First pass: classify every candidate and remember the highest usable quota.
            var knownCount = 0;
            var unknownCount = 0;
            var highestRemaining = long.MinValue;

            foreach (var client in clients)
            {
                if (TryGetRemaining(store, client.Id, now, staleAfter, out var remaining))
                {
                    if (remaining > 0)
                    {
                        knownCount++;
                        if (remaining > highestRemaining)
                        {
                            highestRemaining = remaining;
                        }
                    }
                }
                else
                {
                    unknownCount++;
                }
            }

            var tier = AiQuotaTier.Exhausted;
            if (knownCount > 0)
            {
                tier = AiQuotaTier.Known;
            }
            else if (unknownCount > 0)
            {
                tier = AiQuotaTier.Unknown;
            }

            // Second pass: a uniform reservoir pick within the winning tier. Candidates of the
            // other tiers are skipped, so an exhausted or unknown workspace can never win over
            // one that is known to have credits left.
            IClient? selected = null;
            long? selectedRemaining = null;
            var seen = 0;

            foreach (var client in clients)
            {
                var hasRemaining = TryGetRemaining(store, client.Id, now, staleAfter, out var remaining);

                bool inTier;
                if (tier == AiQuotaTier.Known)
                {
                    inTier = hasRemaining && remaining == highestRemaining;
                }
                else if (tier == AiQuotaTier.Unknown)
                {
                    inTier = hasRemaining == false;
                }
                else
                {
                    inTier = hasRemaining && remaining <= 0;
                }

                if (inTier == false)
                {
                    continue;
                }

                seen++;
                if (random.Next(seen) == 0)
                {
                    selected = client;
                    selectedRemaining = hasRemaining ? remaining : null;
                }
            }

            if (selected == null)
            {
                return null;
            }

            return new AiQuotaSelection(selected, tier, selectedRemaining);
        }

        /// <summary>
        /// Gets the remaining credits of a workspace when the probed value may still be used.
        /// </summary>
        private static bool TryGetRemaining(
            AiQuotaStore store,
            string clientId,
            DateTimeOffset now,
            TimeSpan staleAfter,
            out long remainingMilliCredits)
        {
            if (store.TryGet(clientId, out var snapshot) && snapshot.TryGetRemaining(now, staleAfter, out remainingMilliCredits))
            {
                return true;
            }

            remainingMilliCredits = 0;
            return false;
        }
    }
}
