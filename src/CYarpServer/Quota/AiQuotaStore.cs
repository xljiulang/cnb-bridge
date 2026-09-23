using System;
using System.Collections.Concurrent;
using System.Diagnostics.CodeAnalysis;

namespace CYarpServer.Quota
{
    /// <summary>
    /// In memory quota snapshots of every workspace that has been probed.
    /// <para>
    /// A failed probe keeps the value of the last success together with its timestamp and only
    /// records the reason, so a transient error can neither hide a healthy workspace nor make an
    /// exhausted one look healthy.
    /// </para>
    /// </summary>
    public sealed class AiQuotaStore
    {
        private readonly ConcurrentDictionary<string, AiQuotaSnapshot> _snapshots = new(StringComparer.Ordinal);

        /// <summary>
        /// Records a successful probe.
        /// </summary>
        /// <param name="clientId">The workspace id (the CNB repo slug).</param>
        /// <param name="totalMilliCredits">The organization's allowance for the month, in milli-credits.</param>
        /// <param name="remainingMilliCredits">Remaining credits in milli-credits.</param>
        /// <param name="now">When the quota was read.</param>
        public void ReportSuccess(string clientId, long totalMilliCredits, long remainingMilliCredits, DateTimeOffset now)
        {
            this._snapshots[clientId] = new AiQuotaSnapshot(totalMilliCredits, remainingMilliCredits, now, null);
        }

        /// <summary>
        /// Records a failed probe without discarding the value of the last success.
        /// </summary>
        /// <param name="clientId">The workspace id (the CNB repo slug).</param>
        /// <param name="error">Reason of the failure.</param>
        public void ReportFailure(string clientId, string error)
        {
            this._snapshots.AddOrUpdate(
                clientId,
                _ => new AiQuotaSnapshot(null, null, null, error),
                (_, existing) => existing with { Error = error });
        }

        /// <summary>
        /// Tries to get the snapshot of a workspace.
        /// </summary>
        /// <param name="clientId">The workspace id (the CNB repo slug).</param>
        /// <param name="snapshot">The snapshot when the method returns true.</param>
        /// <returns>true when the workspace has been probed at least once.</returns>
        public bool TryGet(string clientId, [NotNullWhen(true)] out AiQuotaSnapshot? snapshot)
        {
            return this._snapshots.TryGetValue(clientId, out snapshot);
        }
    }
}
