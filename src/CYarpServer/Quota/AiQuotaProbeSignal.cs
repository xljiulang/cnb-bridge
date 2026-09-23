using System.Collections.Generic;
using System.Threading;
using System.Threading.Channels;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 工作空间上线事件的投递通道：状态监听只负责把 clientId 放进来，探测服务负责消费。
    /// <para>
    /// 存在的意义是解耦。CYarp 的状态回调走的是「取一个状态 → 持久化一个」的串行消费者，
    /// 若在回调里直接探测（最长 <c>AiQuota:Timeout</c>），后续的上下线事件都会被堵在队列里；
    /// 而把投递与探测分开后，回调恒为同步返回。
    /// </para>
    /// <para>
    /// 本类型刻意做成一枚叶子：不依赖任何服务。CYarp 的依赖链是
    /// <c>ClientManager → ClientStateChannel → IEnumerable&lt;IClientStateStorage&gt;</c>，
    /// 而 <c>IClientViewer</c> 又由工厂指向 <c>ClientManager</c>，所以状态监听一旦在构造函数里
    /// 索要 <c>IClientViewer</c> 就会成环。叶子类型没有这个问题。
    /// </para>
    /// </summary>
    public sealed class AiQuotaProbeSignal
    {
        /// <summary>
        /// 待探测的工作空间。无界队列：投递方（状态回调）绝不能因背压而阻塞，
        /// 而消费方是单条循环，突发上限也只是「同时上线的数量」这个量级。
        /// </summary>
        private readonly Channel<string> _connected = Channel.CreateUnbounded<string>(
            new UnboundedChannelOptions { SingleReader = true });

        /// <summary>
        /// 投递一个刚上线的工作空间 id。同步返回、绝不阻塞投递方。
        /// </summary>
        /// <param name="clientId">刚上线的工作空间 id，即 CNB 仓库 slug。</param>
        public void NotifyConnected(string clientId)
        {
            this._connected.Writer.TryWrite(clientId);
        }

        /// <summary>
        /// 读取上线事件，供探测服务消费。
        /// </summary>
        /// <param name="cancellationToken">主机关闭时触发。</param>
        /// <returns>工作空间 id 的异步序列。</returns>
        public IAsyncEnumerable<string> ReadAllAsync(CancellationToken cancellationToken)
        {
            return this._connected.Reader.ReadAllAsync(cancellationToken);
        }
    }
}
