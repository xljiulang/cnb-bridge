using System;

namespace CYarpServer
{
    /// <summary>
    /// Options of the AI quota probe. The server periodically reads the CNB organization quota
    /// of every connected workspace through that workspace's own tunnel, and the bare
    /// <c>POST /v1/chat/completions</c> route then prefers the workspace that has the most
    /// remaining credits.
    /// </summary>
    public sealed class AiQuotaOptions
    {
        /// <summary>
        /// The configuration section these options are bound from.
        /// </summary>
        public const string SectionName = "AiQuota";

        /// <summary>
        /// Gets or sets whether the probe runs. Read once at startup, so switching it off in
        /// appsettings.json needs a restart. While it is off the quota of <c>GET /</c> stays
        /// unknown and the bare chat route keeps picking a workspace uniformly at random.
        /// </summary>
        public bool Enabled { get; set; } = true;

        /// <summary>
        /// Gets or sets the probe period, 2 minutes by default. A tick whose previous round is
        /// still running is skipped instead of being queued.
        /// </summary>
        public TimeSpan Interval { get; set; } = TimeSpan.FromMinutes(2d);

        /// <summary>
        /// Gets or sets how long a reading may be reused, 6 minutes by default - three probe
        /// rounds, so a couple of consecutive failures do not drop a workspace out of routing.
        /// An older snapshot - including one that never succeeded - counts as unknown, which only
        /// deprioritises the workspace: an outdated value is never mistaken for a used up quota.
        /// </summary>
        public TimeSpan StaleAfter { get; set; } = TimeSpan.FromMinutes(6d);

        /// <summary>
        /// Gets or sets the timeout of the two charge requests of a single workspace, together.
        /// </summary>
        public TimeSpan Timeout { get; set; } = TimeSpan.FromSeconds(10d);
    }
}
