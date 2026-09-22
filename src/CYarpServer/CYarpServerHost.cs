using CYarp.Server;
using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Options;
using System;
using System.Collections.Generic;
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

            // CYarp server components. Redis backed client state storage is intentionally
            // not registered: this deployment is a single node and StackExchange.Redis is
            // not Native AOT friendly.
            builder.Services.AddCYarp().Configure(builder.Configuration.GetSection(nameof(CYarpOptions)));

            // Strongly typed public surface options, bound with the source generated binder.
            builder.Services.Configure<CnbProxyOptions>(builder.Configuration);

            var app = builder.Build();

            app.UseCYarp();

            // Optional inbound gate for the public OpenAI-compatible API.
            var proxyKey = app.Services.GetRequiredService<IOptions<CnbProxyOptions>>().Value.ProxyKey;
            if (string.IsNullOrEmpty(proxyKey) == false)
            {
                app.Use(CreateProxyKeyGate(proxyKey));
            }

            app.MapCYarp(GetClientId);

            // Explicit RequestDelegate mapping: a delegate with injected complex types
            // makes the Request Delegate Generator treat them as JSON bodies, which
            // fails at runtime under Native AOT.
            app.Map("/{**catchall}", (RequestDelegate)HttpForwardHandler.HandleAsync);

            return app;
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

        /// <summary>
        /// Resolves the client id from the handshake header, falling back to the shared
        /// default used by a single-workspace deployment.
        /// </summary>
        public static string GetClientId(HttpContext context)
        {
            var clientId = context.Request.Headers[ClientIdHeader].ToString();
            return string.IsNullOrEmpty(clientId) ? DefaultClientId : clientId;
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
