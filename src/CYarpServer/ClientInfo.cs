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
        /// Gets or sets when the workspace's <em>current</em> tunnel was established, as the
        /// server sees it (<see cref="CYarp.Server.IClient.CreationTime"/>).
        /// <para>
        /// This is the live tunnel, not the first time the workspace was ever seen: a workspace
        /// that reconnects (the client retries every 5s/10s, and a recycled workspace restarts
        /// the client) gets a fresh value, so a value that keeps moving back and forth is the
        /// signature of a flapping tunnel.
        /// </para>
        /// <para>
        /// Rendered in <c>+08:00</c>, like <see cref="UpdatedAt"/>: every time in this response uses
        /// that one fixed offset rather than the host's local zone, so the payload reads the same
        /// whether the server runs on a Windows box in UTC+8 or on a minimal Linux image that
        /// defaults to UTC.
        /// </para>
        /// </summary>
        public DateTimeOffset ConnectedAt { get; set; }

        /// <summary>
        /// Gets or sets the organization's whole AI allowance for the month, in credits - the pool
        /// <see cref="RemainingCredits"/> is drawn from (free + paid + privilege quota).
        /// <para>
        /// Reported next to the remainder so a caller can see the ratio without a second request;
        /// the spent part is the difference between the two. Like <see cref="RemainingCredits"/> it
        /// is null until a probe succeeds, and a stale value is still reported.
        /// </para>
        /// </summary>
        public decimal? TotalCredits { get; set; }

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
        /// happened. Rendered in the same fixed <c>+08:00</c> offset as
        /// <see cref="ConnectedAt"/>; the timestamp is stored in UTC and only converted when the
        /// response is written.
        /// </summary>
        public DateTimeOffset? UpdatedAt { get; set; }

        /// <summary>
        /// Gets or sets the reason of the most recent failed probe, or null after a success.
        /// </summary>
        public string? Error { get; set; }
    }
}
