namespace CYarpServer.Quota
{
    /// <summary>
    /// The rule that decided which workspace serves a bare (unified base_url) chat request.
    /// </summary>
    public enum AiQuotaTier
    {
        /// <summary>Quota routing is switched off: a uniform pick, exactly as before this feature.</summary>
        Disabled,

        /// <summary>Picked among the workspaces with the highest remaining credits.</summary>
        Known,

        /// <summary>No usable quota (never probed, or too old): picked uniformly among those.</summary>
        Unknown,

        /// <summary>Every candidate is known to be out of credits: picked uniformly among those.</summary>
        Exhausted,
    }
}
