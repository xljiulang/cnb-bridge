using CYarp.Server;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 一次裸 chat 路由的结果：被选中的工作空间、选中它的规则，以及当时可用的剩余额度。
    /// </summary>
    /// <param name="Client">被选中的工作空间。</param>
    /// <param name="Tier">选中它的规则。</param>
    /// <param name="RemainingMilliCredits">被选中工作空间的剩余额度，单位 milli-credit；未知时为 null。</param>
    public readonly record struct AiQuotaSelection(IClient Client, AiQuotaTier Tier, long? RemainingMilliCredits);
}
