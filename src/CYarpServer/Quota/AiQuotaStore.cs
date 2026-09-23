using System;
using System.Collections.Concurrent;
using System.Diagnostics.CodeAnalysis;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 所有被探测过的工作空间的额度快照，保存在内存里。
    /// <para>
    /// 探测失败时保留上次成功的值及其时间戳，只记录失败原因，因此一次瞬时错误既不会让一个
    /// 健康的工作空间消失，也不会让一个额度耗尽的工作空间看起来健康。
    /// </para>
    /// </summary>
    public sealed class AiQuotaStore
    {
        private readonly ConcurrentDictionary<string, AiQuotaSnapshot> _snapshots = new(StringComparer.Ordinal);

        /// <summary>
        /// 记录一次成功的探测。
        /// </summary>
        /// <param name="clientId">工作空间 id，即 CNB 仓库 slug。</param>
        /// <param name="totalMilliCredits">该组织当月总额度，单位 milli-credit。</param>
        /// <param name="remainingMilliCredits">剩余额度，单位 milli-credit。</param>
        /// <param name="now">读到额度的时刻。</param>
        public void ReportSuccess(string clientId, long totalMilliCredits, long remainingMilliCredits, DateTimeOffset now)
        {
            this._snapshots[clientId] = new AiQuotaSnapshot(totalMilliCredits, remainingMilliCredits, now, null);
        }

        /// <summary>
        /// 记录一次失败的探测，且不丢弃上次成功的值。
        /// </summary>
        /// <param name="clientId">工作空间 id，即 CNB 仓库 slug。</param>
        /// <param name="error">失败原因。</param>
        public void ReportFailure(string clientId, string error)
        {
            this._snapshots.AddOrUpdate(
                clientId,
                _ => new AiQuotaSnapshot(null, null, null, error),
                (_, existing) => existing with { Error = error });
        }

        /// <summary>
        /// 尝试取出某个工作空间的快照。
        /// </summary>
        /// <param name="clientId">工作空间 id，即 CNB 仓库 slug。</param>
        /// <param name="snapshot">方法返回 true 时为该快照。</param>
        /// <returns>该工作空间至少被探测过一次时返回 true。</returns>
        public bool TryGet(string clientId, [NotNullWhen(true)] out AiQuotaSnapshot? snapshot)
        {
            return this._snapshots.TryGetValue(clientId, out snapshot);
        }
    }
}
