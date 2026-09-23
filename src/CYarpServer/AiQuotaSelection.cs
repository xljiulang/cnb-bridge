using CYarp.Server;

namespace CYarpServer
{
    /// <summary>
    /// The workspace a bare chat request was routed to, the rule that picked it, and the
    /// remaining credits that were available for it.
    /// </summary>
    /// <param name="Client">The selected workspace.</param>
    /// <param name="Tier">The rule that selected it.</param>
    /// <param name="RemainingMilliCredits">Remaining credits of the selected workspace in milli-credits; null when unknown.</param>
    public readonly record struct AiQuotaSelection(IClient Client, AiQuotaTier Tier, long? RemainingMilliCredits);
}
