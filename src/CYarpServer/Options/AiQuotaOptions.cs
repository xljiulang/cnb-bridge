using System;

namespace CYarpServer.Options
{
    /// <summary>
    /// AI 额度探测的配置。
    /// <para>
    /// 服务端周期性地经每个在线工作空间自己的隧道读取其 CNB 组织额度，
    /// 裸 <c>POST /v1/chat/completions</c> 随后在剩余额度超过
    /// <see cref="MinRemainingCredits"/> 的那些工作空间之间随机挑选。
    /// </para>
    /// </summary>
    public sealed class AiQuotaOptions
    {
        /// <summary>
        /// 该配置项所绑定的配置节名。
        /// </summary>
        public const string SectionName = "AiQuota";

        /// <summary>
        /// 获取或设置是否运行额度探测。只在启动时读取一次，因此改 appsettings.json 后需要重启。
        /// 关闭时 <c>GET /</c> 的额度始终为未知，裸 chat 路由回到等概率随机挑选。
        /// </summary>
        public bool Enabled { get; set; } = true;

        /// <summary>
        /// 获取或设置探测周期，默认 2 分钟。若上一轮还没跑完，本次触发会被跳过而不是排队。
        /// </summary>
        public TimeSpan Interval { get; set; } = TimeSpan.FromMinutes(2d);

        /// <summary>
        /// 获取或设置一次读数可以被复用的时长，默认 6 分钟 —— 相当于三个探测周期，
        /// 这样连续失败几次也不会把工作空间踢出路由。更旧的快照（含从未成功过的）
        /// 一律视为未知，其唯一后果是该工作空间的优先级下降：过期值绝不会被误当成额度已耗尽。
        /// </summary>
        public TimeSpan StaleAfter { get; set; } = TimeSpan.FromMinutes(6d);

        /// <summary>
        /// 获取或设置裸 chat 选路要求的最小剩余额度，单位 credit，默认 1 —— 判定为严格大于，
        /// 因此剩余恰好等于该值的工作空间不参与择优，只会留在「额度不足」的兜底档。
        /// 设为 0 即「只要还有正额度就算可用」。
        /// </summary>
        public decimal MinRemainingCredits { get; set; } = 1m;

        /// <summary>
        /// 获取或设置单个工作空间那一轮探测的超时（两个 charge 请求合计）。
        /// </summary>
        public TimeSpan Timeout { get; set; } = TimeSpan.FromSeconds(10d);

        /// <summary>
        /// 获取 <see cref="MinRemainingCredits"/> 换算成 milli-credit 后的阈值，用于和快照里的
        /// 原始读数直接比较（1000 milli-credit = 1 credit）。换算只在这里做一次，
        /// 避免选路与文档各写一份。
        /// </summary>
        public long MinRemainingMilliCredits => (long)(this.MinRemainingCredits * 1000m);
    }
}
