using CYarp.Server;
using CYarpServer.Authentication;
using CYarpServer.Forwarding;
using CYarpServer.Options;
using CYarpServer.Quota;
using Microsoft.AspNetCore.Authentication;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Collections.Generic;
using System.Security.Claims;
using System.Security.Cryptography;
using System.Text;
using System.Threading.Tasks;

namespace CYarpServer
{
    /// <summary>
    /// 构建 CYarp 服务端的请求管道。刻意不涉及进程级的事务（ServiceSelf、控制台宿主），
    /// 这样集成测试可以在进程内把它起起来。
    /// </summary>
    public static class CYarpServerHost
    {
        /// <summary>
        /// 无任何配置时的监听地址。要改可以传 <c>--urls http://+:8080</c>、设置 ASPNETCORE_URLS，
        /// 或添加 Kestrel:Endpoints 配置节（将来配 TLS 也是在这里）。
        /// </summary>
        public const string DefaultUrls = "http://+:80";

        /// <summary>
        /// CYarp 客户端在握手时报上自己 client id 用的头。
        /// 公网调用方也可以用它来显式指定某个工作空间。
        /// </summary>
        public const string ClientIdHeader = "X-CYarp-Client";

        /// <summary>
        /// 握手未带该头时使用的 client id。单工作空间部署只跑一个 CYarpClient，
        /// 因此共用一个 id 就够了。
        /// </summary>
        public const string DefaultClientId = "cnb";

        /// <summary>
        /// 创建服务端应用。
        /// </summary>
        /// <param name="args">命令行参数。</param>
        /// <param name="configureBuilder">可选的额外 builder 配置（进程宿主会用到）。</param>
        /// <param name="settings">可选的配置覆盖（测试会用到）。</param>
        /// <returns>构建好的应用。</returns>
        public static WebApplication Create(
            string[] args,
            Action<WebApplicationBuilder>? configureBuilder = null,
            IEnumerable<KeyValuePair<string, string?>>? settings = null)
        {
            var builder = WebApplication.CreateBuilder(args);

            if (settings != null)
            {
                builder.Configuration.AddInMemoryCollection(settings);
            }

            configureBuilder?.Invoke(builder);

            // appsettings.json 与二进制同目录；即使服务是从别的工作目录启动的，这一点也要成立。
            builder.WebHost.UseContentRoot(AppContext.BaseDirectory);

            // 监听地址的优先级：配置（命令行 "--urls"、"Urls" 键）> ASPNETCORE_URLS > 内置默认值。
            // 这里显式解析，是因为 appsettings.json 里的 "Urls" 项否则会把 ASPNETCORE_URLS 遮住。
            builder.WebHost.UseUrls(ResolveUrls(builder.Configuration));

            // CYarp 服务端组件。
            //
            // 配置刻意绑在「本程序集」里：配置绑定的源生成器只拦截它参与编译的调用点，
            // 所以 "AddCYarp().Configure(section)"（包自带的 IConfiguration 重载）会落到反射绑定，
            // 而反射绑定在裁剪后的发布产物里会静默失效
            // （实测：Node=...、Protocols=All、KeepAliveInterval=00:00:50、MaxTunnels=10，
            // 而不是配置里的值）。在我们自己的项目里调用 Configure<CYarpOptions>，
            // 生成器就会生成绑定代码，每个值都能绑上。
            builder.Services.AddCYarp();
            builder.Services.Configure<CYarpOptions>(builder.Configuration.GetSection(nameof(CYarpOptions)));

            // CYarp 握手用请求头携带工作空间身份与 CNB token；
            // 这个方案把它们转成 CYarp 保存在 IClient.User 上的 ClaimsPrincipal。
            builder.Services
                .AddAuthentication(CYarpTunnelAuthenticationHandler.SchemeName)
                .AddScheme<AuthenticationSchemeOptions, CYarpTunnelAuthenticationHandler>(CYarpTunnelAuthenticationHandler.SchemeName, null);

            // 公网接口的强类型配置，用源生成绑定器绑定。
            builder.Services.Configure<CnbProxyOptions>(builder.Configuration);

            // AI 额度探测：裸 chat 路由优先选择剩余额度最多的工作空间，GET / 负责报告读到的结果。
            // 两者共用这个 store。
            builder.Services.Configure<AiQuotaOptions>(builder.Configuration.GetSection(AiQuotaOptions.SectionName));
            builder.Services.AddSingleton<AiQuotaStore>();
            builder.Services.AddHostedService<AiQuotaProbeService>();

            var app = builder.Build();

            app.UseCYarp();
            app.UseAuthentication();

            LogCYarpOptions(app.Services);

            // 公网 OpenAI 兼容 API 的可选入站闸门。它只覆盖 chat 端点：models 由配置本地应答
            // （一份静态列表、不触上游），因此保持开放，好让工作空间的保活探活不带密钥。
            var apiKey = app.Services.GetRequiredService<IOptions<CnbProxyOptions>>().Value.ApiKey;
            if (string.IsNullOrEmpty(apiKey) == false)
            {
                app.Use(CreateApiKeyGate(apiKey));
            }

            app.MapCYarp(context => context.User.FindFirstValue(CnbClaims.ClientId) ?? DefaultClientId);

            // 公网接口是一张字面路由表：工作空间 id 就是 CNB 仓库 slug，因此 "{org}/{repo}"
            // 是两个真正的路由参数，而不是要去解析的 catch-all 前缀。这里没列出的路径都由框架
            // 回 404，从而保证路由表与已发布的 API 一一对应。
            //
            // 处理器显式映射为 RequestDelegate：若委托带有注入的复杂类型，
            // Request Delegate Generator 会把它们当成 JSON 请求体。
            app.MapMethods("/{org}/{repo}/v1/models", [HttpMethods.Get], (RequestDelegate)HttpForwardHandler.HandleModelsAsync);
            app.MapMethods("/{org}/{repo}/v1/chat/completions", [HttpMethods.Post], (RequestDelegate)HttpForwardHandler.HandleChatAsync);

            // 裸路径：即「统一 base_url」形态。不知道（或不想硬编码）工作空间 slug 的调用方，
            // 把 base_url 指向服务端根即可：
            //   GET  /                     -> 在线工作空间及其额度
            //   GET  /v1/models            -> 配置的模型列表（从不转发）
            //   POST /v1/chat/completions  -> 转发给剩余额度最多的在线工作空间
            app.MapMethods("/", [HttpMethods.Get], (RequestDelegate)HttpForwardHandler.HandleClientsAsync);
            app.MapMethods("/v1/models", [HttpMethods.Get], (RequestDelegate)HttpForwardHandler.HandleModelsAsync);
            app.MapMethods("/v1/chat/completions", [HttpMethods.Post], (RequestDelegate)HttpForwardHandler.HandleChatAsync);

            return app;
        }

        /// <summary>
        /// 打印真正生效的 CYarp 配置。将来若某个 CYarp 版本或裁剪器改动让生成的绑定回退到
        /// 库默认值，这一行会立刻暴露出来，而不是静默出错。
        /// </summary>
        private static void LogCYarpOptions(IServiceProvider services)
        {
            var options = services.GetRequiredService<IOptions<CYarpOptions>>().Value;
            var logger = services.GetRequiredService<ILoggerFactory>().CreateLogger("CYarpServer");

            logger.LogInformation(
                "CYarp options in effect: KeepAlive={keepAlive}, KeepAliveInterval={keepAliveInterval}, MaxTunnelsPerClient={maxTunnelsPerClient}, CreationTimeout={creationTimeout}",
                options.Client.KeepAlive,
                options.Client.KeepAliveInterval,
                options.HttpTunnel.MaxTunnelsPerClient,
                options.HttpTunnel.CreationTimeout);
        }

        /// <summary>
        /// 解析监听地址：命令行 <c>--urls</c> / 配置里的 <c>Urls</c> 优先，
        /// 其次是 ASPNETCORE_URLS 与 DOTNET_URLS，最后回落到 <see cref="DefaultUrls"/>。
        /// </summary>
        /// <param name="configuration">应用配置。</param>
        /// <returns>要监听的地址。</returns>
        private static string ResolveUrls(IConfiguration configuration)
        {
            var configured = configuration["urls"];
            if (string.IsNullOrWhiteSpace(configured))
            {
                configured = Environment.GetEnvironmentVariable("ASPNETCORE_URLS");
            }
            if (string.IsNullOrWhiteSpace(configured))
            {
                configured = Environment.GetEnvironmentVariable("DOTNET_URLS");
            }

            return string.IsNullOrWhiteSpace(configured) ? DefaultUrls : configured;
        }

        /// <summary>
        /// 构造 ApiKey 入站闸门：chat 端点必须带正确的 Bearer key，其余免鉴权端点直接放行。
        /// </summary>
        /// <param name="apiKey">配置的公网 key。</param>
        /// <returns>中间件委托。</returns>
        private static Func<HttpContext, RequestDelegate, Task> CreateApiKeyGate(string apiKey)
        {
            return async (context, next) =>
            {
                // 不归 ApiKey 管的：
                // - CYarp 端点：它们的 Authorization 头带的是 CNB token，从来不是公网 ApiKey；
                // - GET .../v1/models（裸路径与带前缀两种）：由配置本地应答、不需要凭据，
                //   工作空间的保活 cron 会不带密钥地探它；
                // - GET /（在线工作空间列表）：本地发现用的辅助接口。
                if (IsCYarpEndpoint(context.Request.Path) || IsModelsEndpoint(context) || IsClientsEndpoint(context))
                {
                    await next(context);
                    return;
                }

                const string scheme = "Bearer ";
                var header = context.Request.Headers.Authorization.ToString();
                var token = header.StartsWith(scheme, StringComparison.OrdinalIgnoreCase) ? header[scheme.Length..].Trim() : string.Empty;
                if (FixedTimeEquals(token, apiKey) == false)
                {
                    context.Response.StatusCode = StatusCodes.Status401Unauthorized;
                    return;
                }

                await next(context);
            };
        }

        /// <summary>
        /// 获取该路径是否指向 CYarp 端点："/cyarp" 或 "/cyarp/"（客户端握手）与
        /// "/cyarp/{tunnelId}"（隧道）。tunnelId 是 GUID，因此 "/cyarp/" 之下还有别的段的路径
        /// 是公网 API 路径 —— 只是它的工作空间 id 恰好以 "cyarp" 开头（例如
        /// "/cyarp/repo/v1/chat/completions"）—— 这类路径仍然必须带上 ApiKey。
        /// </summary>
        /// <param name="path">请求路径。</param>
        /// <returns>是 CYarp 端点时返回 true。</returns>
        private static bool IsCYarpEndpoint(PathString path)
        {
            if (path.StartsWithSegments("/cyarp", out var remainder) == false)
            {
                return false;
            }

            return remainder.HasValue == false || remainder.Value!.AsSpan(1).IndexOf('/') < 0;
        }

        /// <summary>
        /// 获取该请求是否为 <c>GET .../v1/models</c> —— 裸 <c>/v1/models</c> 或带工作空间前缀的
        /// <c>/{org}/{repo}/v1/models</c>。这份列表由本地应答，因此刻意开放
        /// （见 <see cref="CreateApiKeyGate"/>）。
        /// </summary>
        /// <param name="context">当前请求上下文。</param>
        /// <returns>是 models 端点时返回 true。</returns>
        private static bool IsModelsEndpoint(HttpContext context)
        {
            return HttpMethods.IsGet(context.Request.Method)
                && context.Request.Path.Value is { } path
                && path.EndsWith("/v1/models", StringComparison.OrdinalIgnoreCase);
        }

        /// <summary>
        /// 获取该请求是否为 <c>GET /</c>，即在线工作空间列表。与模型列表一样刻意开放：
        /// 它只暴露有哪些工作空间在线。
        /// </summary>
        /// <param name="context">当前请求上下文。</param>
        /// <returns>是列表端点时返回 true。</returns>
        private static bool IsClientsEndpoint(HttpContext context)
        {
            return HttpMethods.IsGet(context.Request.Method)
                && (context.Request.Path.Value is null or "/" or "");
        }

        /// <summary>
        /// 以固定耗时比较两个字符串，避免因逐字符短路比较而泄漏时序信息。
        /// </summary>
        /// <param name="left">左侧字符串。</param>
        /// <param name="right">右侧字符串。</param>
        /// <returns>两者相等时返回 true。</returns>
        private static bool FixedTimeEquals(string left, string right)
        {
            var leftBytes = Encoding.UTF8.GetBytes(left);
            var rightBytes = Encoding.UTF8.GetBytes(right);
            return leftBytes.Length == rightBytes.Length && CryptographicOperations.FixedTimeEquals(leftBytes, rightBytes);
        }
    }
}
