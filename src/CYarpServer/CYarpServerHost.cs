using CYarp.Server;
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

            var app = builder.Build();

            app.UseCYarp();
            app.UseAuthentication();

            LogCYarpOptions(app.Services);

            // Optional inbound gate for the public OpenAI-compatible API.
            var proxyKey = app.Services.GetRequiredService<IOptions<CnbProxyOptions>>().Value.ProxyKey;
            if (string.IsNullOrEmpty(proxyKey) == false)
            {
                app.Use(CreateProxyKeyGate(proxyKey));
            }

            app.MapCYarp(context => context.User.FindFirstValue(CnbClaims.ClientId) ?? DefaultClientId);

            // Explicit RequestDelegate mapping: a delegate with injected complex types
            // would make the Request Delegate Generator treat them as JSON bodies.
            app.Map("/{**catchall}", (RequestDelegate)HttpForwardHandler.HandleAsync);

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

        private static Func<HttpContext, RequestDelegate, Task> CreateProxyKeyGate(string proxyKey)
        {
            return async (context, next) =>
            {
                // The CYarp handshake carries the CNB token in its Authorization header,
                // never the public ProxyKey.
                if (context.Request.Path.StartsWithSegments("/cyarp"))
                {
                    await next(context);
                    return;
                }

                const string scheme = "Bearer ";
                var header = context.Request.Headers.Authorization.ToString();
                var token = header.StartsWith(scheme, StringComparison.OrdinalIgnoreCase) ? header[scheme.Length..].Trim() : string.Empty;
                if (FixedTimeEquals(token, proxyKey) == false)
                {
                    context.Response.StatusCode = StatusCodes.Status401Unauthorized;
                    return;
                }

                await next(context);
            };
        }

        private static bool FixedTimeEquals(string left, string right)
        {
            var leftBytes = Encoding.UTF8.GetBytes(left);
            var rightBytes = Encoding.UTF8.GetBytes(right);
            return leftBytes.Length == rightBytes.Length && CryptographicOperations.FixedTimeEquals(leftBytes, rightBytes);
        }
    }
}
