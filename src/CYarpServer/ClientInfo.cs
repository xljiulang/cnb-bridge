using System;

namespace CYarpServer
{
    /// <summary>
    /// One entry of the <c>GET /</c> response: a connected workspace and what the server knows
    /// about its AI quota.
    /// </summary>
    public sealed class ClientInfo
    {
        /// <summary>
        /// Gets or sets the workspace id, i.e. the CNB repo slug (e.g. <c>org/repo</c>).
        /// </summary>
        public required string ClientId { get; set; }

        /// <summary>
        /// Gets or sets whether the workspace's tunnel is connected. Always true for now, since
        /// only connected workspaces are listed; the field keeps the shape stable for a future
        /// list that also reports recently disconnected ones.
        /// </summary>
        public bool Online { get; set; }

        /// <summary>
        /// Gets or sets the remaining AI credits of the workspace's organization, in credits
        /// (value / 1000 of the raw milli-credits, which is the unit of the CNB charge API).
        /// Null when no quota has been read yet.
        /// <para>
        /// A stale value is still reported: it is last known data, and <see cref="Stale"/> plus
        /// <see cref="Error"/> tell the caller that routing no longer trusts it.
        /// </para>
        /// </summary>
        public decimal? RemainingCredits { get; set; }

        /// <summary>
        /// Gets or sets whether the reading is too old to be used for routing. Always true while
        /// no probe has ever succeeded.
        /// </summary>
        public bool Stale { get; set; }

        /// <summary>
        /// Gets or sets when the quota was last read successfully, or null if that never
        /// happened.
        /// </summary>
        public DateTimeOffset? UpdatedAt { get; set; }

        /// <summary>
        /// Gets or sets the reason of the most recent failed probe, or null after a success.
        /// </summary>
        public string? Error { get; set; }
    }
}
