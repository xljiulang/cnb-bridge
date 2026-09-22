using CYarp.Server.Clients;
using CYarp.Server.Features;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Http.Features;
using Microsoft.Extensions.DependencyInjection;
using System.Globalization;
using System.Threading.Tasks;

namespace CYarp.Server.Middlewares
{
    /// <summary>
    /// Handles the tunnel between client and target for proxied connections.
    /// <para>
    /// CNB-CN delta: upstream used a minimal API delegate with injected parameters.
    /// Request delegate generation runs per assembly, so a library cannot rely on it;
    /// the handler therefore takes <see cref="HttpContext"/> only and resolves its
    /// dependencies from <see cref="HttpContext.RequestServices"/>. This keeps the
    /// Native AOT publish free of runtime code generation.
    /// </para>
    /// </summary>
    static class TunnelHandler
    {
        /// <summary>
        /// Tunnels do not require authentication and authorization, the tunnelId is secured by its validity.
        /// </summary>
        public static async Task HandleTunnelAsync(HttpContext context)
        {
            var tunnelFactory = context.RequestServices.GetRequiredService<TunnelFactory>();
            var cyarpFeature = context.Features.GetRequiredFeature<ICYarpFeature>();
            if (cyarpFeature.IsCYarpRequest == false)
            {
                TunnelLog.LogInvalidRequest(tunnelFactory.Logger, context.Connection.Id, "Invalid CYarp request");
                await Results.StatusCode(StatusCodes.Status400BadRequest).ExecuteAsync(context);
                return;
            }

            var tunnelId = GetTunnelId(context);
            if (tunnelId.IsValid == false || tunnelFactory.Contains(tunnelId) == false)
            {
                TunnelLog.LogInvalidTunnelId(tunnelFactory.Logger, context.Connection.Id, tunnelId);
                await Results.StatusCode(StatusCodes.Status403Forbidden).ExecuteAsync(context);
                return;
            }

            var stream = await cyarpFeature.AcceptAsStreamAsync();
            var httpTunnel = new Tunnel(stream, tunnelId, cyarpFeature.Protocol);

            if (tunnelFactory.SetResult(httpTunnel))
            {
                await httpTunnel.WaitForDisposeAsync();
            }
            else
            {
                await httpTunnel.DisposeAsync();
            }

            // Close the connection of the tunnel
            context.Abort();
        }

        private static TunnelId GetTunnelId(HttpContext context)
        {
            return context.Request.RouteValues.TryGetValue("tunnelId", out var value)
                && TunnelId.TryParse(value as string, CultureInfo.InvariantCulture, out var tunnelId)
                ? tunnelId
                : TunnelId.Empty;
        }
    }
}
