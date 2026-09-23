using System;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// <c>GET /</c> 响应里的一项：一个在线工作空间，以及服务端所知道的它的 AI 额度。
    /// </summary>
    public sealed class ClientInfo
    {
        /// <summary>
        /// 获取或设置工作空间 id，即 CNB 仓库 slug（例如 <c>org/repo</c>）。
        /// </summary>
        public required string ClientId { get; set; }

        /// <summary>
        /// 获取或设置该工作空间<em>当前这条</em>隧道建立的时刻，取自服务端所见
        /// （<see cref="CYarp.Server.IClient.CreationTime"/>）。
        /// <para>
        /// 它指的是当前这条活着的隧道，而不是该工作空间第一次被看到的时间：工作空间重连
        /// （客户端每 5s/10s 重试，被回收后重建会重启客户端）会得到一个全新的值，
        /// 所以这个值反复跳动就说明隧道在抖动。
        /// </para>
        /// <para>
        /// 与 <see cref="UpdatedAt"/> 一样按 <c>+08:00</c> 输出：本响应里所有时间都用这同一个
        /// 固定偏移，而不跟随主机时区，这样不管服务端跑在 UTC+8 的 Windows 机器上，
        /// 还是跑在默认 UTC 的精简 Linux 镜像上，返回的内容读起来都一样。
        /// </para>
        /// </summary>
        public DateTimeOffset ConnectedAt { get; set; }

        /// <summary>
        /// 获取或设置该组织当月的 AI 总额度，单位 credit —— 也就是
        /// <see cref="RemainingCredits"/> 所从这个池子里扣（免费 + 付费 + 特权额度）。
        /// <para>
        /// 与剩余额度并列返回，调用方不必再发一次请求就能看出占比；已用部分即两者之差。
        /// 与 <see cref="RemainingCredits"/> 一样，在第一次探测成功前为 null，
        /// 而过期的读数仍会照常返回。
        /// </para>
        /// </summary>
        public decimal? TotalCredits { get; set; }

        /// <summary>
        /// 获取或设置该工作空间所属组织的剩余 AI 额度，单位 credit
        /// （即 CNB charge API 的原始 milli-credit 值 / 1000）。尚未读到额度时为 null。
        /// <para>
        /// 过期的读数仍会照常返回：它是最后一次已知的数据，而 <see cref="Stale"/> 与
        /// <see cref="Error"/> 会告诉调用方路由已不再采信它。
        /// </para>
        /// </summary>
        public decimal? RemainingCredits { get; set; }

        /// <summary>
        /// 获取或设置这份读数是否太旧、已不能用于路由。在从未探测成功过时恒为 true。
        /// </summary>
        public bool Stale { get; set; }

        /// <summary>
        /// 获取或设置最后一次成功读到额度的时刻；从未成功过则为 null。与
        /// <see cref="ConnectedAt"/> 用同一个固定 <c>+08:00</c> 偏移输出；该时刻内部按 UTC
        /// 保存，只在写响应时转换。
        /// </summary>
        public DateTimeOffset? UpdatedAt { get; set; }

        /// <summary>
        /// 获取或设置最近一次探测失败的原因；成功后回到 null。
        /// </summary>
        public string? Error { get; set; }
    }
}
