using CYarp.Server.Clients;
using CYarp.Server.Features;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Net;
using System.Threading.Tasks;
using Yarp.ReverseProxy.Forwarder;

namespace CYarp.Server.Middlewares
{
    /// <summary>
    /// Handler for IClient.
    /// <para>
    /// CNB-CN delta: upstream used a minimal API delegate with injected parameters and
    /// <c>Results.Forbid()</c>. This version takes <see cref="HttpContext"/> only and
    /// returns a plain 403, because the deployment registers no authentication services
    /// (see the CNB token flow) and delegate parameter binding is not Native AOT friendly.
    /// </para>
    /// </summary>
    sealed class ClientHandler
    {
        private const string CYarpTargetUriHeader = "CYarp-TargetUri";
        private readonly Func<HttpContext, ValueTask<string?>> clientIdProvider;

        public ClientHandler(Func<HttpContext, ValueTask<string?>> clientIdProvider)
        {
            this.clientIdProvider = clientIdProvider;
        }

        public async Task HandleClientAsync(HttpContext context)
        {
            var services = context.RequestServices;
            var tunnelFactory = services.GetRequiredService<TunnelFactory>();
            var clientManager = services.GetRequiredService<ClientManager>();
            var httpForwarder = services.GetRequiredService<IHttpForwarder>();
            var options = services.GetRequiredService<IOptionsMonitor<CYarpOptions>>().CurrentValue;
            var logger = services.GetRequiredService<ILoggerFactory>().CreateLogger<Client>();

            var cyarpFeature = context.Features.GetRequiredFeature<ICYarpFeature>();
            if (cyarpFeature.IsCYarpRequest == false)
            {
                ClientLog.LogInvalidRequest(logger, context.Connection.Id, "Not a valid CYarp request");
                await Results.StatusCode(StatusCodes.Status400BadRequest).ExecuteAsync(context);
                return;
            }

            if (context.Request.Headers.TryGetValue(CYarpTargetUriHeader, out var targetUri) == false)
            {
                ClientLog.LogInvalidRequest(logger, context.Connection.Id, $"Header {CYarpTargetUriHeader} not found");
                await Results.StatusCode(StatusCodes.Status400BadRequest).ExecuteAsync(context);
                return;
            }

            // Validate CYarp-TargetUri format
            if (Uri.TryCreate(targetUri, UriKind.Absolute, out var clientTargetUri) == false)
            {
                ClientLog.LogInvalidRequest(logger, context.Connection.Id, $"Header {CYarpTargetUriHeader} value is not a Uri");
                await Results.StatusCode(StatusCodes.Status400BadRequest).ExecuteAsync(context);
                return;
            }

            // Find clientId
            var clientId = await this.clientIdProvider.Invoke(context);
            if (string.IsNullOrEmpty(clientId))
            {
                ClientLog.LogInvalidRequest(logger, context.Connection.Id, "Unable to obtain IClient Id");
                await Results.StatusCode(StatusCodes.Status403Forbidden).ExecuteAsync(context);
                return;
            }

            var stream = await cyarpFeature.AcceptAsSafeWriteStreamAsync();
            var connection = new ClientConnection(clientId, stream, options.Client, logger);

            // CNB-CN delta: the client reports the upstream credential in the handshake's
            // Authorization header (ConnectHeaders["Authorization"] = "Bearer $CNB_TOKEN").
            // This deployment does not consume that header for its own authentication
            // (no JWT), so the value is kept verbatim and replayed to the upstream.
            var upstreamAuthorization = context.Request.Headers.Authorization.ToString();

            await using (var client = new Client(connection, httpForwarder, tunnelFactory, options.HttpTunnel, clientTargetUri, upstreamAuthorization, cyarpFeature.Protocol, context.User))
            {
                var httpConnection = context.Connection;
                if (httpConnection != null && httpConnection.RemoteIpAddress != null)
                {
                    client.RemoteEndpoint = new IPEndPoint(httpConnection.RemoteIpAddress, httpConnection.RemotePort);
                }

                if (await clientManager.AddAsync(client))
                {
                    ClientLog.LogConnected(logger, clientId, cyarpFeature.Protocol, clientManager.Count);
                    var reason = await client.WaitForCloseAsync();

                    if (await clientManager.RemoveAsync(client))
                    {
                        ClientLog.LogDisconnected(logger, clientId, cyarpFeature.Protocol, reason.ToString(), clientManager.Count);
                    }
                }
            }

            // Close connection
            context.Abort();
        }
    }
}
