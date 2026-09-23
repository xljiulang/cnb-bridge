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
    /// Builds the CYarp server pipeline. Kept free of process level concerns
    /// (ServiceSelf, console host) so integration tests can start it in-process.
    /// </summary>
    public static class CYarpServerHost
    {
        /// <summary>
        /// Listen address used when nothing else is configured. Change it by passing
        /// <c>--urls http://+:8080</c>, by setting ASPNETCORE_URLS, or by adding a
        /// Kestrel:Endpoints section (which is also where TLS is configured later).
        /// </summary>
        public const string DefaultUrls = "http://+:80";

        /// <summary>
        /// Header the CYarp client uses to report its client id during the handshake.
        /// Public callers may also use it to address a workspace explicitly.
        /// </summary>
        public const string ClientIdHeader = "X-CYarp-Client";

        /// <summary>
        /// Client id used when the handshake omits the header. A single workspace
        /// deployment runs a single CYarpClient, so one shared id is enough.
        /// </summary>
        public const string DefaultClientId = "cnb";

        /// <summary>
        /// Creates the server application.
        /// </summary>
        /// <param name="args">Command line arguments.</param>
        /// <param name="configureBuilder">Optional extra builder configuration (used by the process host).</param>
        /// <param name="settings">Optional configuration overrides (used by tests).</param>
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

            // appsettings.json lives next to the binary, also when the service is started
            // from another working directory.
            builder.WebHost.UseContentRoot(AppContext.BaseDirectory);

            // Listen address precedence: configuration (command line "--urls", "Urls" key),
            // then ASPNETCORE_URLS, then the built-in default. Resolved explicitly because a
            // "Urls" entry in appsettings.json would otherwise shadow ASPNETCORE_URLS.
            builder.WebHost.UseUrls(ResolveUrls(builder.Configuration));

            // CYarp server components.
            //
            // The options are bound here, in this assembly, on purpose: the configuration
            // binding source generator only intercepts calls it compiles, so
            // "AddCYarp().Configure(section)" (the package's own IConfiguration overload) ends
            // up as the reflection binder, which is silently a no-op in the trimmed publish
            // (measured: Node=..., Protocols=All, KeepAliveInterval=00:00:50, MaxTunnels=10
            // instead of the configured values). Calling Configure<CYarpOptions> from our own
            // project makes the generator emit the binding code and every value binds.
            builder.Services.AddCYarp();
            builder.Services.Configure<CYarpOptions>(builder.Configuration.GetSection(nameof(CYarpOptions)));

            // The CYarp handshake carries the workspace identity and the CNB token as headers;
            // this scheme turns them into the ClaimsPrincipal that CYarp keeps on IClient.User.
            builder.Services
                .AddAuthentication(CYarpTunnelAuthenticationHandler.SchemeName)
                .AddScheme<AuthenticationSchemeOptions, CYarpTunnelAuthenticationHandler>(CYarpTunnelAuthenticationHandler.SchemeName, null);

            // Strongly typed public surface options, bound with the source generated binder.
            builder.Services.Configure<CnbProxyOptions>(builder.Configuration);

            // AI quota probing: the routing of the bare chat route prefers the workspace with the
            // most remaining credits, and GET / reports what was read. Both share this store.
            builder.Services.Configure<AiQuotaOptions>(builder.Configuration.GetSection(AiQuotaOptions.SectionName));
            builder.Services.AddSingleton<AiQuotaStore>();
            builder.Services.AddHostedService<AiQuotaProbeService>();

            var app = builder.Build();

            app.UseCYarp();
            app.UseAuthentication();

            LogCYarpOptions(app.Services);

            // Optional inbound gate for the public OpenAI-compatible API. It only covers the
            // chat endpoint: models is answered locally from configuration (a static list with
            // no upstream call), so it stays open for the workspace keepalive probe.
            var apiKey = app.Services.GetRequiredService<IOptions<CnbProxyOptions>>().Value.ApiKey;
            if (string.IsNullOrEmpty(apiKey) == false)
            {
                app.Use(CreateApiKeyGate(apiKey));
            }

            app.MapCYarp(context => context.User.FindFirstValue(CnbClaims.ClientId) ?? DefaultClientId);

            // The public surface is a literal route table: the workspace id is the CNB repo
            // slug, so "{org}/{repo}" are two real route parameters instead of a catch-all
            // prefix to parse. Everything not listed here is answered 404 by the framework,
            // which keeps the route table equal to the published API.
            //
            // Handlers are mapped as explicit RequestDelegate: a delegate with injected
            // complex types would make the Request Delegate Generator treat them as JSON bodies.
            app.MapMethods("/{org}/{repo}/v1/models", [HttpMethods.Get], (RequestDelegate)HttpForwardHandler.HandleModelsAsync);
            app.MapMethods("/{org}/{repo}/v1/chat/completions", [HttpMethods.Post], (RequestDelegate)HttpForwardHandler.HandleChatAsync);

            // Bare routes: the "unified base_url" surface. A caller that does not know (or does
            // not want to hardcode) the workspace slug points base_url at the server root:
            //   GET  /                     -> connected workspace ids, e.g. ["org/repo"]
            //   GET  /v1/models            -> the configured model list (never forwarded)
            //   POST /v1/chat/completions  -> forwarded to one connected workspace at random
            app.MapMethods("/", [HttpMethods.Get], (RequestDelegate)HttpForwardHandler.HandleClientsAsync);
            app.MapMethods("/v1/models", [HttpMethods.Get], (RequestDelegate)HttpForwardHandler.HandleModelsAsync);
            app.MapMethods("/v1/chat/completions", [HttpMethods.Post], (RequestDelegate)HttpForwardHandler.HandleChatAsync);

            return app;
        }

        /// <summary>
        /// Logs the CYarp options that actually took effect. If a future CYarp release or
        /// trimmer change makes the generated binding fall back to the library defaults, this
        /// line shows it immediately instead of failing silently.
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

        private static Func<HttpContext, RequestDelegate, Task> CreateApiKeyGate(string apiKey)
        {
            return async (context, next) =>
            {
                // Not behind the ApiKey:
                // - the CYarp endpoints: their Authorization header carries the CNB token,
                //   never the public ApiKey;
                // - GET .../v1/models (both the bare and the workspace-addressed route): answered
                //   locally from configuration, needs no credential, and the workspace keepalive
                //   cron probes it without one;
                // - GET / (the connected workspace list): a local discovery helper.
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
        /// Gets whether the path addresses a CYarp endpoint: "/cyarp" or "/cyarp/" (the client
        /// handshake) and "/cyarp/{tunnelId}" (the tunnel). tunnelId is a GUID, so a path with
        /// another segment below "/cyarp/" is a public API path whose workspace id happens to
        /// start with "cyarp" (e.g. "/cyarp/repo/v1/chat/completions") and must still present
        /// the ApiKey.
        /// </summary>
        private static bool IsCYarpEndpoint(PathString path)
        {
            if (path.StartsWithSegments("/cyarp", out var remainder) == false)
            {
                return false;
            }

            return remainder.HasValue == false || remainder.Value!.AsSpan(1).IndexOf('/') < 0;
        }

        /// <summary>
        /// Gets whether the request is <c>GET .../v1/models</c> - either the bare
        /// <c>/v1/models</c> or the workspace-addressed <c>/{org}/{repo}/v1/models</c>. That list
        /// is answered locally, so it is deliberately open (see <see cref="CreateApiKeyGate"/>).
        /// </summary>
        private static bool IsModelsEndpoint(HttpContext context)
        {
            return HttpMethods.IsGet(context.Request.Method)
                && context.Request.Path.Value is { } path
                && path.EndsWith("/v1/models", StringComparison.OrdinalIgnoreCase);
        }

        /// <summary>
        /// Gets whether the request is <c>GET /</c>, the connected workspace list. Deliberately
        /// open, like the model list: it exposes only which workspaces are online.
        /// </summary>
        private static bool IsClientsEndpoint(HttpContext context)
        {
            return HttpMethods.IsGet(context.Request.Method)
                && (context.Request.Path.Value is null or "/" or "");
        }

        private static bool FixedTimeEquals(string left, string right)
        {
            var leftBytes = Encoding.UTF8.GetBytes(left);
            var rightBytes = Encoding.UTF8.GetBytes(right);
            return leftBytes.Length == rightBytes.Length && CryptographicOperations.FixedTimeEquals(leftBytes, rightBytes);
        }
    }
}
