using CYarp.Server;
using CYarpServer.Authentication;
using CYarpServer.Contracts;
using CYarpServer.Options;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Collections.Concurrent;
using System.Collections.Generic;
using System.IO;
using System.Net;
using System.Net.Http;
using System.Security.Claims;
using System.Text.Json;
using System.Text.Json.Serialization.Metadata;
using System.Threading;
using System.Threading.Tasks;

namespace CYarpServer.Quota
{
    /// <summary>
    /// 周期性地读取每个在线工作空间的 CNB AI 额度。
    /// <para>
    /// charge 端点只应答来自 CNB 网络内部的请求，所以探测不是由服务端直接调用它们：
    /// 而是向每个工作空间要一条 TCP 隧道（<see cref="IClient.CreateTcpTunnelAsync"/>），
    /// 再在这条隧道上发一个普通的 HTTPS 请求。TLS 与 HTTP 都由
    /// <see cref="SocketsHttpHandler"/> 在隧道流之上完成，与 CYarp 自己转发 HTTP 请求的方式一致。
    /// </para>
    /// <para>
    /// 这里的一切都不允许影响隧道本身：失败只会被记进快照的错误字段，并保留上一次的值，
    /// 因此最坏的情况也不过是某个工作空间的额度一直是未知。
    /// </para>
    /// <para>
    /// 探测有两个触发源：一是「启动后立刻 + 每 <see cref="AiQuotaOptions.Interval"/> 一轮」的节拍，
    /// 二是 <see cref="AiQuotaProbeSignal"/> 送来的「某工作空间刚上线」事件。后者让新连上的工作空间
    /// 不必等一整个周期就能出现在 <c>GET /</c> 里并参与额度优选；前者继续作为兜底。
    /// </para>
    /// </summary>
    public sealed partial class AiQuotaProbeService : BackgroundService
    {
        /// <summary>
        /// CNB OpenAPI 的媒体类型；charge 端点并不声明支持 <c>application/json</c>。
        /// </summary>
        private const string AcceptMediaType = "application/vnd.cnb.api+json";

        private readonly IClientViewer _viewer;
        private readonly AiQuotaStore _store;
        private readonly AiQuotaProbeSignal _signal;
        private readonly AiQuotaOptions _options;
        private readonly ILogger<AiQuotaProbeService> _logger;

        /// <summary>
        /// 正在被探测的工作空间 id。节拍轮次与上线事件可能同时盯上同一个工作空间，若不去重就会
        /// 白占两条隧道（<c>MaxTunnelsPerClient</c> 默认 20）。键的存在即「正在探测」。
        /// </summary>
        private readonly ConcurrentDictionary<string, byte> _probing = new(StringComparer.Ordinal);

        /// <summary>
        /// 创建探测器。
        /// </summary>
        /// <param name="viewer">在线工作空间。</param>
        /// <param name="store">存放读数的地方，供路由与 <c>GET /</c> 使用。</param>
        /// <param name="signal">工作空间上线事件。</param>
        /// <param name="options">探测配置。</param>
        /// <param name="logger">日志组件。</param>
        public AiQuotaProbeService(
            IClientViewer viewer,
            AiQuotaStore store,
            AiQuotaProbeSignal signal,
            IOptions<AiQuotaOptions> options,
            ILogger<AiQuotaProbeService> logger)
        {
            this._viewer = viewer;
            this._store = store;
            this._signal = signal;
            this._options = options.Value;
            this._logger = logger;
        }

        /// <summary>
        /// 跑两条互不干扰的循环：节拍循环（启动后立刻一轮 + 每个
        /// <see cref="AiQuotaOptions.Interval"/> 一轮）与上线事件循环。
        /// <para>
        /// 刻意分成两条而不是合成一条：若共用一条，持续到来的上线事件会把节拍轮次饿死，
        /// 反过来一次慢的节拍轮次也会推迟事件探测。两条循环各管一件事，谁也拖不住谁。
        /// </para>
        /// </summary>
        /// <param name="stoppingToken">主机关闭时触发。</param>
        protected override async Task ExecuteAsync(CancellationToken stoppingToken)
        {
            if (this._options.Enabled == false)
            {
                LogProbeDisabled(this._logger);
                return;
            }

            if (this._options.Interval <= TimeSpan.Zero)
            {
                LogProbeIntervalInvalid(this._logger, this._options.Interval);
                return;
            }

            await Task.WhenAll(
                this.RunIntervalLoopAsync(stoppingToken),
                this.ConsumeConnectedSignalsAsync(stoppingToken));
        }

        /// <summary>
        /// 节拍循环：启动后立刻跑一轮，之后每个 <see cref="AiQuotaOptions.Interval"/> 一轮。
        /// 下一拍之前会先把当前轮 await 完，因此两轮永不重叠，慢的工作空间也不会堆积任务。
        /// </summary>
        /// <param name="stoppingToken">主机关闭时触发。</param>
        private async Task RunIntervalLoopAsync(CancellationToken stoppingToken)
        {
            try
            {
                using var timer = new PeriodicTimer(this._options.Interval);
                await this.ProbeAllAsync(stoppingToken);

                while (await timer.WaitForNextTickAsync(stoppingToken))
                {
                    await this.ProbeAllAsync(stoppingToken);
                }
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                // 正在关闭。
            }
        }

        /// <summary>
        /// 上线事件循环：某个工作空间刚连上，就立刻探它一次，好让额度立即可见而不必等一整个周期。
        /// <para>
        /// 顺序处理，与节拍轮次口径一致（同组织的工作空间共享额度，没必要并发压同一端点）。
        /// 某个慢探测会推迟后面的事件探测，但既不影响隧道转发，也不会堵住 CYarp 的状态管道
        /// —— 投递与探测是分开的，这正是 <see cref="AiQuotaProbeSignal"/> 存在的理由。
        /// </para>
        /// </summary>
        /// <param name="stoppingToken">主机关闭时触发。</param>
        private async Task ConsumeConnectedSignalsAsync(CancellationToken stoppingToken)
        {
            try
            {
                await foreach (var clientId in this._signal.ReadAllAsync(stoppingToken))
                {
                    // 与节拍循环同口径：关掉探测时事件也不该触发任何动作。
                    if (this._options.Enabled == false)
                    {
                        return;
                    }

                    // 事件里只有 id，真实实例得回 viewer 取：CYarp 交给状态监听的是
                    // ReadOnlyClient 包装，它建不了隧道。查不到说明客户端已经断线或被替换。
                    if (this._viewer.TryGetValue(clientId, out var client) == false)
                    {
                        LogProbeSkipped(this._logger, clientId, "the workspace is no longer connected");
                        continue;
                    }

                    LogProbeOnConnected(this._logger, clientId);
                    await this.ProbeOneAsync(client, stoppingToken);
                }
            }
            catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
            {
                // 正在关闭。
            }
        }

        /// <summary>
        /// 顺序地探测每个在线工作空间一次。
        /// <para>
        /// 刻意串行：同一组织下的工作空间共享同一份额度，并行探测只会让同一个端点承受成倍的负载。
        /// </para>
        /// </summary>
        /// <param name="cancellationToken">主机关闭时触发。</param>
        private async Task ProbeAllAsync(CancellationToken cancellationToken)
        {
            // viewer 是并发集合；先快照一份，好让同一轮探测看到稳定的工作空间集合。
            var candidates = new List<IClient>(this._viewer.Count);
            foreach (var client in this._viewer)
            {
                candidates.Add(client);
            }

            foreach (var client in candidates)
            {
                cancellationToken.ThrowIfCancellationRequested();
                await this.ProbeOneAsync(client, cancellationToken);
            }
        }

        /// <summary>
        /// 探测一个工作空间。所有失败都记进快照而不是抛出去，因此单个坏掉的工作空间不会中断整轮。
        /// <para>
        /// 若同一个工作空间已经在被探测（节拍轮次与上线事件撞上），本次直接跳过：与其白占两条隧道，
        /// 不如让先来的那次把结果写进快照。
        /// </para>
        /// </summary>
        /// <param name="client">要探测的工作空间。</param>
        /// <param name="cancellationToken">主机关闭时触发。</param>
        private async Task ProbeOneAsync(IClient client, CancellationToken cancellationToken)
        {
            var clientId = client.Id;

            if (this._probing.TryAdd(clientId, 0) == false)
            {
                LogProbeSkipped(this._logger, clientId, "a probe of this workspace is already running");
                return;
            }

            try
            {
                await this.ProbeOneCoreAsync(client, clientId, cancellationToken);
            }
            finally
            {
                this._probing.TryRemove(clientId, out _);
            }
        }

        /// <summary>
        /// 探测的实际内容。调用前必须已用 <see cref="ProbeOneAsync"/> 取得该工作空间的探测权。
        /// </summary>
        /// <param name="client">要探测的工作空间。</param>
        /// <param name="clientId">该工作空间的 id（调用方已取出，避免重复读取）。</param>
        /// <param name="cancellationToken">主机关闭时触发。</param>
        private async Task ProbeOneCoreAsync(IClient client, string clientId, CancellationToken cancellationToken)
        {
            // 组织名就是仓库 slug 的第一段。不含斜杠的 client id（单工作空间的默认值）
            // 没有组织可言，也就无从问起。
            var separator = clientId.IndexOf('/');
            if (separator <= 0)
            {
                LogProbeSkipped(this._logger, clientId, "the client id has no organization");
                return;
            }

            // 没有 CNB token 就无法认证这次 charge 调用，而服务端绝不会自己造一个。
            var authorization = client.User.FindFirstValue(CnbClaims.UpstreamAuthorization);
            if (string.IsNullOrEmpty(authorization))
            {
                LogProbeSkipped(this._logger, clientId, "the handshake carried no upstream authorization");
                return;
            }

            var organization = clientId[..separator];

            using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
            timeout.CancelAfter(this._options.Timeout);

            try
            {
                var (total, remaining) = await ReadQuotaAsync(client, organization, authorization, timeout.Token);
                var now = DateTimeOffset.UtcNow;

                this._store.ReportSuccess(clientId, total, remaining, now);
                LogProbeSucceeded(this._logger, clientId, organization, remaining, total);
            }
            catch (OperationCanceledException) when (cancellationToken.IsCancellationRequested)
            {
                throw;
            }
            catch (Exception ex)
            {
                var error = Describe(ex);
                this._store.ReportFailure(clientId, error);
                LogProbeFailed(this._logger, clientId, organization, error);
            }
        }

        /// <summary>
        /// 经工作空间自己的隧道读取某个组织的 AI 额度：当月总额度，以及其中剩余的部分。
        /// </summary>
        /// <param name="client">提供隧道的工作空间。</param>
        /// <param name="organization">组织 slug（client id 的第一段）。</param>
        /// <param name="authorization">握手时报上来的 CNB token。</param>
        /// <param name="cancellationToken">两个请求合计的超时。</param>
        /// <returns>当月总额度与剩余额度，单位均为 milli-credit。</returns>
        private static async Task<(long Total, long Remaining)> ReadQuotaAsync(
            IClient client,
            string organization,
            string authorization,
            CancellationToken cancellationToken)
        {
            // 隧道的目标 Uri 是 CNB AI 基址（https://api.cnb.cool/{org}/{repo}/-/ai）；
            // charge 端点与它同主机，因此不需要额外配置地址。
            var target = client.TargetUri;
            var baseAddress = new Uri($"{target.Scheme}://{target.Authority}");

            var handler = new SocketsHttpHandler
            {
                Proxy = null,
                UseProxy = false,
                UseCookies = false,
                AllowAutoRedirect = false,
                // 两个请求共用一条连接就够了，把上限压在 1 意味着这一轮只占一条隧道，而不是每个请求一条。
                MaxConnectionsPerServer = 1,
                ConnectCallback = (_, token) => new ValueTask<Stream>(client.CreateTcpTunnelAsync(token)),
            };

            // invoker 按「每个工作空间、每一轮」创建：它绝不能被共享，否则连接池里的连接
            // 会被复用到另一个工作空间的隧道上。
            using var invoker = new HttpMessageInvoker(handler, disposeHandler: true);

            var escapedOrganization = Uri.EscapeDataString(organization);
            var quotaPath = $"/{escapedOrganization}/-/charge/quota";
            var volumePath = $"/{escapedOrganization}/-/charge/volume";

            var quota = await GetAsync(
                invoker,
                new Uri(baseAddress, quotaPath),
                quotaPath,
                authorization,
                CnbJsonContext.Default.CnbChargeQuotaResponse,
                cancellationToken);

            var volume = await GetAsync(
                invoker,
                new Uri(baseAddress, volumePath),
                volumePath,
                authorization,
                CnbJsonContext.Default.CnbChargeVolumeResponse,
                cancellationToken);

            if (quota.CreditInMilli?.Total is not { } total)
            {
                throw new InvalidOperationException($"{quotaPath} carried no credit_in_milli.total");
            }

            if (volume.CreditInMilli is not { } used)
            {
                throw new InvalidOperationException($"{volumePath} carried no credit_in_milli");
            }

            var frozen = volume.FreezeCreditInMilli ?? 0L;
            return (total, total - used - frozen);
        }

        /// <summary>
        /// 经隧道发一个带认证的 GET 并读取其 JSON 响应体。非成功状态码、读不出的响应体、
        /// 或空的响应体都会以异常形式抛出，由调用方转成快照里的错误。
        /// </summary>
        /// <typeparam name="TResponse">本服务端读取的响应部分。</typeparam>
        /// <param name="invoker">承载隧道的、按工作空间创建的 invoker。</param>
        /// <param name="requestUri">请求的绝对 Uri。</param>
        /// <param name="path">路径，用于错误信息。</param>
        /// <param name="authorization">握手时报上来的 CNB token。</param>
        /// <param name="typeInfo">源生成的 JSON 元数据（发布产物经过裁剪）。</param>
        /// <param name="cancellationToken">请求超时。</param>
        /// <returns>反序列化后的响应。</returns>
        private static async Task<TResponse> GetAsync<TResponse>(
            HttpMessageInvoker invoker,
            Uri requestUri,
            string path,
            string authorization,
            JsonTypeInfo<TResponse> typeInfo,
            CancellationToken cancellationToken)
            where TResponse : class
        {
            using var request = new HttpRequestMessage(HttpMethod.Get, requestUri);
            request.Headers.TryAddWithoutValidation("Accept", AcceptMediaType);
            request.Headers.TryAddWithoutValidation("Authorization", authorization);

            using var response = await invoker.SendAsync(request, cancellationToken);
            if (response.IsSuccessStatusCode == false)
            {
                throw new HttpRequestException(
                    $"GET {path} returned {(int)response.StatusCode} {response.ReasonPhrase}",
                    inner: null,
                    response.StatusCode);
            }

            await using var stream = await response.Content.ReadAsStreamAsync(cancellationToken);
            var payload = await JsonSerializer.DeserializeAsync(stream, typeInfo, cancellationToken);
            if (payload is null)
            {
                throw new InvalidOperationException($"GET {path} returned an empty body");
            }

            return payload;
        }

        /// <summary>
        /// 把异常渲染成快照用的单行错误信息。
        /// </summary>
        /// <param name="exception">异常。</param>
        /// <returns>简短描述；有 HTTP 状态码时会带上。</returns>
        private static string Describe(Exception exception)
        {
            if (exception is HttpRequestException httpException && httpException.StatusCode is { } statusCode)
            {
                return $"{(int)statusCode} {httpException.Message}";
            }

            if (exception is OperationCanceledException)
            {
                return "the request timed out";
            }

            return exception.Message;
        }

        [LoggerMessage(Level = LogLevel.Information, Message = "AI quota probe is disabled (AiQuota:Enabled=false)")]
        private static partial void LogProbeDisabled(ILogger logger);

        [LoggerMessage(Level = LogLevel.Warning, Message = "AI quota probe is disabled: AiQuota:Interval={interval} is not positive")]
        private static partial void LogProbeIntervalInvalid(ILogger logger, TimeSpan interval);

        [LoggerMessage(Level = LogLevel.Debug, Message = "AI quota probe skipped workspace '{clientId}': {reason}")]
        private static partial void LogProbeSkipped(ILogger logger, string clientId, string reason);

        [LoggerMessage(Level = LogLevel.Debug, Message = "AI quota probe of workspace '{clientId}' triggered by its connect event")]
        private static partial void LogProbeOnConnected(ILogger logger, string clientId);

        [LoggerMessage(Level = LogLevel.Information, Message = "AI quota of workspace '{clientId}' (organization {organization}): {remainingMilliCredits} of {totalMilliCredits} milli-credits remaining")]
        private static partial void LogProbeSucceeded(ILogger logger, string clientId, string organization, long remainingMilliCredits, long totalMilliCredits);

        [LoggerMessage(Level = LogLevel.Warning, Message = "AI quota probe of workspace '{clientId}' (organization {organization}) failed: {error}")]
        private static partial void LogProbeFailed(ILogger logger, string clientId, string organization, string error);
    }
}
