using CYarp.Server;
using System.Threading;
using System.Threading.Tasks;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 借助 CYarp 的客户端状态监听，把「工作空间上线」变成一个立刻探测额度的信号。
    /// <para>
    /// 这里只投递 clientId，探测由 <see cref="AiQuotaProbeService"/> 去做。原因是 CYarp 用一个
    /// 串行消费者依次调用各 state storage，回调里若内联探测（最长 <c>AiQuota:Timeout</c>），
    /// 后面的上下线事件就都排在它后面了 —— 而这个 channel 的设计初衷恰恰是
    /// 「确保持久层的性能不影响到 ClientManager」。所以本类型的两个方法都恒为同步完成。
    /// </para>
    /// <para>
    /// 它借的是「状态变化通知」这个能力，并不承担任何持久化：没有落盘，重启后也不恢复。
    /// </para>
    /// </summary>
    public sealed class AiQuotaProbeTrigger(AiQuotaProbeSignal signal) : IClientStateStorage
    {
        /// <summary>
        /// 空操作。该方法的语义是「把所有客户端初始化为离线」，属于持久化场景的启动清场，
        /// 与本特性无关。
        /// </summary>
        /// <param name="cancellationToken">主机关闭时触发。</param>
        /// <returns>已完成的任务。</returns>
        public Task InitClientStatesAsync(CancellationToken cancellationToken)
        {
            return Task.CompletedTask;
        }

        /// <summary>
        /// 状态变化时投递上线信号。下线不做任何事：快照继续保留，而 <c>GET /</c> 本来就只列在线
        /// 工作空间；重连会被当成一次新的上线事件，于是重新探测。
        /// <para>
        /// 只传 id 而不传 <see cref="ClientState.Client"/>：那是个
        /// <c>ReadOnlyClient</c> 包装，其 <c>CreateTcpTunnelAsync</c> 直接抛异常，探测必须回
        /// <c>IClientViewer</c> 取真实实例。
        /// </para>
        /// </summary>
        /// <param name="clientState">CYarp 上报的客户端状态。</param>
        /// <param name="cancellationToken">主机关闭时触发。</param>
        /// <returns>已完成的任务。</returns>
        public Task WriteClientStateAsync(ClientState clientState, CancellationToken cancellationToken)
        {
            if (clientState.IsConnected)
            {
                signal.NotifyConnected(clientState.Client.Id);
            }

            return Task.CompletedTask;
        }
    }
}
