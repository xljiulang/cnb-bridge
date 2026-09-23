using CYarp.Server;
using System;
using System.Collections.Generic;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 依据服务端探测到的额度，为裸（统一 base_url）chat 请求挑选工作空间：剩余额度超过阈值的
    /// 那些之间等概率随机分摊；只有在没有任何一个超阈值时，才退化为在「未知」的那些之间随机挑选。
    /// </summary>
    public static class AiQuotaRouter
    {
        /// <summary>
        /// 挑选一个工作空间；完全没有候选时返回 null。
        /// <para>
        /// 候选被分成三档：<see cref="AiQuotaTier.Available"/>（额度新且大于阈值）、
        /// <see cref="AiQuotaTier.Unknown"/>（从未探测过或读数过期）、
        /// <see cref="AiQuotaTier.Insufficient"/>（额度新且不超过阈值，含已耗尽）。
        /// 第一个非空的档位说了算，且档内等概率挑选 —— 因此额度 3 credit 与 300 credit 的工作空间
        /// 被选中的概率相同，兜底档位之间同样能分摊流量。关闭额度路由时所有候选同属一档，
        /// 等价于本特性之前的行为。
        /// </para>
        /// </summary>
        /// <param name="clients">在线工作空间（快照；viewer 会并发变动）。</param>
        /// <param name="store">探测到的额度。</param>
        /// <param name="quotaRoutingEnabled">额度路由是否开启。</param>
        /// <param name="now">当前时刻。</param>
        /// <param name="staleAfter">可用快照的最大年龄。</param>
        /// <param name="minRemainingMilliCredits">算作「额度充足」的最小剩余额度（单位 milli-credit，严格大于）。</param>
        /// <param name="random">随机源，便于测试固定种子。</param>
        /// <returns>选中结果；<paramref name="clients"/> 为空时返回 null。</returns>
        public static AiQuotaSelection? Pick(
            IReadOnlyList<IClient> clients,
            AiQuotaStore store,
            bool quotaRoutingEnabled,
            DateTimeOffset now,
            TimeSpan staleAfter,
            long minRemainingMilliCredits,
            Random random)
        {
            if (clients.Count == 0)
            {
                return null;
            }

            if (quotaRoutingEnabled == false)
            {
                return new AiQuotaSelection(clients[random.Next(clients.Count)], AiQuotaTier.Disabled, null);
            }

            // 第一趟：给每个候选分档。
            var availableCount = 0;
            var unknownCount = 0;

            foreach (var client in clients)
            {
                if (TryGetRemaining(store, client.Id, now, staleAfter, out var remaining) == false)
                {
                    unknownCount++;
                }
                else if (remaining > minRemainingMilliCredits)
                {
                    availableCount++;
                }
            }

            var tier = AiQuotaTier.Insufficient;
            if (availableCount > 0)
            {
                tier = AiQuotaTier.Available;
            }
            else if (unknownCount > 0)
            {
                tier = AiQuotaTier.Unknown;
            }

            // 第二趟：在胜出档位内做等概率蓄水池抽样。其它档位的候选会被跳过，
            // 因此额度不足或未知的工作空间，绝不会胜过任何一个额度充足的工作空间。
            IClient? selected = null;
            long? selectedRemaining = null;
            var seen = 0;

            foreach (var client in clients)
            {
                var hasRemaining = TryGetRemaining(store, client.Id, now, staleAfter, out var remaining);

                bool inTier;
                if (tier == AiQuotaTier.Available)
                {
                    inTier = hasRemaining && remaining > minRemainingMilliCredits;
                }
                else if (tier == AiQuotaTier.Unknown)
                {
                    inTier = hasRemaining == false;
                }
                else
                {
                    inTier = hasRemaining && remaining <= minRemainingMilliCredits;
                }

                if (inTier == false)
                {
                    continue;
                }

                seen++;
                if (random.Next(seen) == 0)
                {
                    selected = client;
                    selectedRemaining = hasRemaining ? remaining : null;
                }
            }

            if (selected == null)
            {
                return null;
            }

            return new AiQuotaSelection(selected, tier, selectedRemaining);
        }

        /// <summary>
        /// 在探测值仍可用时，取出某个工作空间的剩余额度。
        /// </summary>
        private static bool TryGetRemaining(
            AiQuotaStore store,
            string clientId,
            DateTimeOffset now,
            TimeSpan staleAfter,
            out long remainingMilliCredits)
        {
            if (store.TryGet(clientId, out var snapshot) && snapshot.TryGetRemaining(now, staleAfter, out remainingMilliCredits))
            {
                return true;
            }

            remainingMilliCredits = 0;
            return false;
        }
    }
}
