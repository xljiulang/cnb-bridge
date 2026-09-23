namespace CYarpServer.Quota
{
    /// <summary>
    /// 裸（统一 base_url）chat 请求最终按哪条规则选中了工作空间。
    /// </summary>
    public enum AiQuotaTier
    {
        /// <summary>额度路由已关闭：等概率随机挑选，与本特性之前完全一致。</summary>
        Disabled,

        /// <summary>剩余额度超过阈值：在这些工作空间之间等概率挑选。</summary>
        Available,

        /// <summary>没有可用的额度（从未探测过，或读数已过期）：在这些工作空间中等概率挑选。</summary>
        Unknown,

        /// <summary>所有候选都已知额度不足（不超过阈值，含已耗尽）：在这些工作空间中等概率挑选。</summary>
        Insufficient,
    }
}
