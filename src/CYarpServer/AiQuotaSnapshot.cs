using System;

namespace CYarpServer
{
    /// <summary>
    /// What the server knows about the AI quota of one workspace: the last remaining credits it
    /// read successfully, when that happened, and the reason of the most recent failure.
    /// </summary>
    /// <param name="RemainingMilliCredits">Remaining credits in milli-credits (1000 = 1 credit); null until a probe succeeds.</param>
    /// <param name="UpdatedAt">When the quota was last read successfully; null if that never happened.</param>
    /// <param name="Error">Reason of the most recent failed probe; null after a success.</param>
    public sealed record AiQuotaSnapshot(long? RemainingMilliCredits, DateTimeOffset? UpdatedAt, string? Error)
    {
        /// <summary>
        /// Gets whether this snapshot is too old to be trusted for routing. A snapshot that
        /// never succeeded is always stale.
        /// </summary>
        /// <param name="now">Current time.</param>
        /// <param name="staleAfter">Maximum age of a usable snapshot.</param>
        /// <returns>true when the snapshot has to be treated as unknown.</returns>
        public bool IsStale(DateTimeOffset now, TimeSpan staleAfter)
        {
            if (this.UpdatedAt is not { } updatedAt)
            {
                return true;
            }

            return now - updatedAt > staleAfter;
        }

        /// <summary>
        /// Tries to get the remaining credits for routing. Succeeds only for a value that is
        /// known and fresh enough; a failed probe keeps the previous value but also its
        /// timestamp, so an outdated value is reported as unknown instead of being reused.
        /// </summary>
        /// <param name="now">Current time.</param>
        /// <param name="staleAfter">Maximum age of a usable snapshot.</param>
        /// <param name="remainingMilliCredits">The remaining credits in milli-credits when the method returns true.</param>
        /// <returns>true when a usable remaining credit value is available.</returns>
        public bool TryGetRemaining(DateTimeOffset now, TimeSpan staleAfter, out long remainingMilliCredits)
        {
            if (this.RemainingMilliCredits is { } remaining && this.IsStale(now, staleAfter) == false)
            {
                remainingMilliCredits = remaining;
                return true;
            }

            remainingMilliCredits = 0;
            return false;
        }
    }
}
