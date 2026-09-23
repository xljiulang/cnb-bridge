using CYarp.Server;
using CYarpServer.Authentication;
using CYarpServer.Contracts;
using CYarpServer.Options;
using CYarpServer.Quota;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Security.Claims;
using System.Threading.Tasks;

namespace CYarpServer.Forwarding
{
    /// <summary>
    /// 公网 OpenAI 兼容接口的处理器。路由在 <see cref="CYarpServerHost"/> 里注册，分两种形态：
    /// <list type="bullet">
    /// <item>带工作空间前缀：<c>/{org}/{repo}/v1/models</c> 与
    /// <c>/{org}/{repo}/v1/chat/completions</c> —— 工作空间 id 就是 CNB 仓库 slug，
    /// 由两个路由参数拼成，因此这里不做任何路径解析；</item>
    /// <item>裸路径（统一 base_url）：<c>GET /</c> 列出在线工作空间及其 AI 额度，
    /// <c>GET /v1/models</c> 由配置应答，
    /// <c>POST /v1/chat/completions</c> 挑选剩余 AI 额度最多的在线工作空间。</item>
    /// </list>
    /// </summary>
    public static partial class HttpForwardHandler
    {
        /// <summary>
        /// 响应里所有时间戳统一使用的偏移（<see cref="ClientInfo.ConnectedAt"/> 与
        /// <see cref="ClientInfo.UpdatedAt"/>）。刻意固定：主机本地时区在 UTC+8 的 Windows 机器
        /// 与默认 UTC 的精简 Linux 镜像之间并不相同，若跟随主机，同一字段在不同部署下
        /// 就会表示不同的墙上时间。
        /// </summary>
        private static readonly TimeSpan ResponseTimeOffset = TimeSpan.FromHours(8d);

        /// <summary>
        /// 把响应里的时间戳规整到固定的 <c>+08:00</c> 偏移，同时保持时刻本身不变。
        /// 响应携带的每个时间都会过这里，因此调用方不必在一条载荷里去调和两种偏移。
        /// <para>
        /// 之所以单独抽成一个方法，是因为它区分的那件事实在 UTC+8 主机上看不出来，
        /// 只有换到别处才暴露：<see cref="DateTimeOffset.ToOffset"/> 会连墙上时间一起搬
        /// （03:29Z 变成 11:29+08:00），而用 <c>DateTime</c> 重建则只是给同一个墙上时间换标签，
        /// 会悄悄把时刻平移掉「主机时区与目标时区之差」那么多。
        /// </para>
        /// </summary>
        /// <param name="time">时间戳，其偏移任意。</param>
        /// <returns>同一个时刻，改以 +08:00 表示。</returns>
        internal static DateTimeOffset ToResponseTime(DateTimeOffset time)
        {
            return time.ToOffset(ResponseTimeOffset);
        }

        /// <summary>
        /// <c>GET /</c> —— 在线工作空间，以及各自最后一次读到的 AI 额度，
        /// 以 <see cref="ClientInfo"/> 的 JSON 数组返回；无在线时为 <c>[]</c>。
        /// 本地应答、无需凭据：它是发现用的辅助接口，不是代理端点。
        /// <para>
        /// 只列在线的工作空间；额度值是探测最后一次成功读到的那个：
        /// <see cref="ClientInfo.Stale"/> 与 <see cref="ClientInfo.Error"/> 说明路由是否仍然采信它。
        /// 列表按剩余额度倒序，额度多的在前。
        /// </para>
        /// </summary>
        public static Task HandleClientsAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var store = services.GetRequiredService<AiQuotaStore>();
            var options = services.GetRequiredService<IOptions<AiQuotaOptions>>().Value;

            var clients = ProjectClients(Snapshot(viewer), store, options, DateTimeOffset.UtcNow);

            return context.Response.WriteAsJsonAsync(clients, CnbJsonContext.Default.ClientInfoArray);
        }

        /// <summary>
        /// 把一个在线工作空间及其探测到的额度投影成响应对象。到这里隧道按定义就是活的
        /// —— 只有在线工作空间才会被列出 —— 因此「在线」这件事以隧道建立的时刻来表示。
        /// </summary>
        /// <param name="client">在线工作空间。</param>
        /// <param name="store">探测到的额度。</param>
        /// <param name="options">探测配置，用于过期阈值。</param>
        /// <param name="now">当前时刻。</param>
        /// <returns>该工作空间的响应对象。</returns>
        internal static ClientInfo ToClientInfo(IClient client, AiQuotaStore store, AiQuotaOptions options, DateTimeOffset now)
        {
            var clientId = client.Id;

            var connectedAt = ToResponseTime(client.CreationTime);

            if (store.TryGet(clientId, out var snapshot) == false)
            {
                return new ClientInfo
                {
                    ClientId = clientId,
                    ConnectedAt = connectedAt,
                    RemainingCredits = null,
                    TotalCredits = null,
                    Stale = true,
                    UpdatedAt = null,
                    Error = null,
                };
            }

            return new ClientInfo
            {
                ClientId = clientId,
                ConnectedAt = connectedAt,
                RemainingCredits = snapshot.RemainingMilliCredits is { } remaining ? remaining / 1000m : null,
                TotalCredits = snapshot.TotalMilliCredits is { } total ? total / 1000m : null,
                Stale = snapshot.IsStale(now, options.StaleAfter),
                UpdatedAt = snapshot.UpdatedAt is { } updatedAt ? ToResponseTime(updatedAt) : null,
                Error = snapshot.Error,
            };
        }

        /// <summary>
        /// 把在线工作空间投影成 <c>GET /</c> 的响应对象，并按剩余额度倒序排列。
        /// <para>
        /// 顺序键是「剩余额度降序 → clientId 升序」。<see cref="decimal"/> 的默认比较器把
        /// <c>null</c> 视为最小值，因此降序时从未读到额度（或只探测失败过）的那些自然落到最后
        /// —— 这是刻意依赖比较器语义，并非漏了兜底。过期的旧读数不做特殊对待、照常按数值参与排序：
        /// 这份列表表达的是「账面上的数字从大到小」，读数是否还值得采信由
        /// <see cref="ClientInfo.Stale"/> 与 <see cref="ClientInfo.Error"/> 自行表达。
        /// 额度为负（组织已超额）时自然排在 0 之后、<c>null</c> 之前。
        /// </para>
        /// <para>
        /// 次键取 clientId 是为了输出稳定：同一组织下的多个工作空间额度必然相同，而「全部未知」时
        /// 更是所有项都并列，没有次键的话这些常见情形的顺序将无法预期。
        /// </para>
        /// </summary>
        /// <param name="clients">在线工作空间。</param>
        /// <param name="store">探测到的额度。</param>
        /// <param name="options">探测配置，用于过期阈值。</param>
        /// <param name="now">当前时刻。</param>
        /// <returns>排序后的响应对象数组。</returns>
        internal static ClientInfo[] ProjectClients(
            IReadOnlyList<IClient> clients,
            AiQuotaStore store,
            AiQuotaOptions options,
            DateTimeOffset now)
        {
            return clients
                .Select(client => ToClientInfo(client, store, options, now))
                .OrderByDescending(client => client.RemainingCredits)
                .ThenBy(client => client.ClientId, StringComparer.Ordinal)
                .ToArray();
        }

        /// <summary>
        /// <c>GET /{org}/{repo}/v1/models</c> —— 由配置本地应答，从不转发。
        /// 该工作空间隧道断开时返回 503，这正是保活 cron 用来识别「假死」的信号；
        /// 两种情况都不消耗 AI 额度。
        /// <para>
        /// 裸 <c>GET /v1/models</c> 复用同一个处理器，但刻意不检查隧道状态：没有工作空间前缀时，
        /// 它是一份静态的「本服务端能提供什么」的列表，因此恒返回 200。
        /// </para>
        /// </summary>
        public static Task HandleModelsAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var options = services.GetRequiredService<IOptions<CnbProxyOptions>>().Value;

            if (TryGetClientId(context, out var clientId))
            {
                var viewer = services.GetRequiredService<IClientViewer>();
                if (viewer.TryGetValue(clientId, out _) == false)
                {
                    LogWorkspaceNotConnected(
                        services.GetRequiredService<ILoggerFactory>().CreateLogger(LogCategory),
                        clientId,
                        StatusCodes.Status503ServiceUnavailable,
                        context.Request.Path.Value ?? "/");

                    context.Response.StatusCode = StatusCodes.Status503ServiceUnavailable;
                    return Task.CompletedTask;
                }
            }

            var models = options.ProxyModels is { Length: > 0 } configured ? configured : CnbProxyOptions.DefaultModels;
            var payload = new ModelListResponse
            {
                Data = models.Select(model => new ModelItem { Id = model, OwnedBy = "cnb" }).ToArray(),
            };

            return context.Response.WriteAsJsonAsync(payload, CnbJsonContext.Default.ModelListResponse);
        }

        /// <summary>
        /// <c>POST /{org}/{repo}/v1/chat/completions</c> —— 经该工作空间的隧道转发。
        /// 调用方的凭据（公网 ApiKey）会被丢弃，替换成 CYarp 客户端握手时报上来的 CNB token。
        /// <para>
        /// 裸 <c>POST /v1/chat/completions</c> 复用同一个处理器，但没有工作空间前缀：
        /// 它挑选剩余 AI 额度最多的在线工作空间（见 <see cref="AiQuotaRouter.Pick"/>），
        /// 无在线工作空间时返回 502。
        /// </para>
        /// </summary>
        public static async Task HandleChatAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var viewer = services.GetRequiredService<IClientViewer>();
            var logger = services.GetRequiredService<ILoggerFactory>().CreateLogger(LogCategory);

            IClient? client;
            if (TryGetClientId(context, out var clientId))
            {
                if (viewer.TryGetValue(clientId, out client) == false)
                {
                    LogWorkspaceNotConnected(logger, clientId, StatusCodes.Status502BadGateway, context.Request.Path.Value ?? "/");
                    context.Response.StatusCode = StatusCodes.Status502BadGateway;
                    return;
                }
            }
            else
            {
                var options = services.GetRequiredService<IOptions<AiQuotaOptions>>().Value;
                var store = services.GetRequiredService<AiQuotaStore>();
                var selection = AiQuotaRouter.Pick(
                    Snapshot(viewer),
                    store,
                    options.Enabled,
                    DateTimeOffset.UtcNow,
                    options.StaleAfter,
                    Random.Shared);

                if (selection is not { } picked)
                {
                    LogNoConnectedWorkspace(logger, context.Request.Path.Value ?? "/");
                    context.Response.StatusCode = StatusCodes.Status502BadGateway;
                    return;
                }

                client = picked.Client;
                LogRouted(logger, picked.Client.Id, picked.Tier, picked.RemainingMilliCredits, viewer.Count);
            }

            // CNB token 就放在 CYarp 保存在 IClient.User 上的握手身份里。
            var upstreamAuthorization = client.User.FindFirstValue(CnbClaims.UpstreamAuthorization);

            var stopwatch = Stopwatch.StartNew();
            var error = await client.ForwardHttpAsync(context, new CnbAiTransformer(upstreamAuthorization));
            stopwatch.Stop();

            if (error == Yarp.ReverseProxy.Forwarder.ForwarderError.None)
            {
                LogResponse(logger, client.Id, context.Request.Method, context.Response.StatusCode, stopwatch.Elapsed);
            }
            else
            {
                LogForwardError(logger, client.Id, context.Request.Method, error);
            }
        }

        /// <summary>
        /// 把在线工作空间复制进一个列表，好让路由决策基于一个稳定的集合，
        /// 因为 viewer 这个集合会随隧道的来去而变动。
        /// </summary>
        /// <param name="viewer">在线工作空间。</param>
        /// <returns>工作空间快照。</returns>
        private static IReadOnlyList<IClient> Snapshot(IClientViewer viewer)
        {
            var clients = new List<IClient>(viewer.Count);
            foreach (var client in viewer)
            {
                clients.Add(client);
            }

            return clients;
        }

        /// <summary>
        /// 从 <c>/{org}/{repo}</c> 路由参数里读出工作空间 id。对裸（统一 base_url）路由返回
        /// false，因为它们没有工作空间前缀。
        /// </summary>
        private static bool TryGetClientId(HttpContext context, out string clientId)
        {
            if (context.Request.RouteValues.TryGetValue("org", out var org) && org is string orgValue && orgValue.Length > 0
                && context.Request.RouteValues.TryGetValue("repo", out var repo) && repo is string repoValue && repoValue.Length > 0)
            {
                clientId = $"{orgValue}/{repoValue}";
                return true;
            }

            clientId = string.Empty;
            return false;
        }

        private const string LogCategory = "CYarpServer.HttpForward";

        [LoggerMessage(Level = LogLevel.Warning, Message = "Workspace '{clientId}' is not connected, {statusCode} returned for {path}")]
        private static partial void LogWorkspaceNotConnected(ILogger logger, string clientId, int statusCode, string path);

        [LoggerMessage(Level = LogLevel.Warning, Message = "No workspace is connected, 502 returned for {path}")]
        private static partial void LogNoConnectedWorkspace(ILogger logger, string path);

        [LoggerMessage(Level = LogLevel.Information, Message = "Routed to workspace '{clientId}' ({tier}, {remainingMilliCredits} milli-credits remaining) out of {count} connected")]
        private static partial void LogRouted(ILogger logger, string clientId, AiQuotaTier tier, long? remainingMilliCredits, int count);

        [LoggerMessage(Level = LogLevel.Information, Message = "<= [{clientId}] {method} {statusCode}, elapsed {elapsed}")]
        private static partial void LogResponse(ILogger logger, string clientId, string method, int statusCode, TimeSpan elapsed);

        [LoggerMessage(Level = LogLevel.Error, Message = "=> [{clientId}] {method} forwarding failed with {error}")]
        private static partial void LogForwardError(ILogger logger, string clientId, string method, Yarp.ReverseProxy.Forwarder.ForwarderError error);
    }
}
