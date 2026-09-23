using System;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 服务端关于某个工作空间 AI 额度的已知信息：最后一次成功读到的额度（组织总额度与
    /// 其中剩余的部分）、读到的时间，以及最近一次失败的原因。
    /// </summary>
    /// <param name="TotalMilliCredits">该组织当月总额度，单位 milli-credit（免费 + 付费 + 特权）；探测成功前为 null。</param>
    /// <param name="RemainingMilliCredits">剩余额度，单位 milli-credit（1000 = 1 credit）；探测成功前为 null。</param>
    /// <param name="UpdatedAt">最后一次成功读到额度的时刻；从未成功过则为 null。</param>
    /// <param name="Error">最近一次探测失败的原因；成功后为 null。</param>
    public sealed record AiQuotaSnapshot(long? TotalMilliCredits, long? RemainingMilliCredits, DateTimeOffset? UpdatedAt, string? Error)
    {
        /// <summary>
        /// 获取该快照是否已旧到不能用于路由。从未成功过的快照恒为过期。
        /// </summary>
        /// <param name="now">当前时刻。</param>
        /// <param name="staleAfter">可用快照的最大年龄。</param>
        /// <returns>当该快照必须按「未知」处理时返回 true。</returns>
        public bool IsStale(DateTimeOffset now, TimeSpan staleAfter)
        {
            if (this.UpdatedAt is not { } updatedAt)
            {
                return true;
            }

            return now - updatedAt > staleAfter;
        }

        /// <summary>
        /// 尝试取出可用于路由的剩余额度。只有已知且足够新的值才会成功；探测失败时保留上一次的
        /// 值、但也保留它的时间戳，因此过期值会被报成未知，而不会被继续复用。
        /// </summary>
        /// <param name="now">当前时刻。</param>
        /// <param name="staleAfter">可用快照的最大年龄。</param>
        /// <param name="remainingMilliCredits">方法返回 true 时，为剩余额度（单位 milli-credit）。</param>
        /// <returns>存在可用的剩余额度时返回 true。</returns>
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
