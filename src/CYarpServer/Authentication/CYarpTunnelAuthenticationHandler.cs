using Microsoft.AspNetCore.Authentication;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System.Collections.Generic;
using System.Security.Claims;
using System.Text.Encodings.Web;
using System.Threading.Tasks;

namespace CYarpServer.Authentication
{
    /// <summary>
    /// Authentication scheme of the CYarp client handshake.
    /// <para>
    /// The CYarp client reports its identity and its upstream credential as handshake
    /// headers (ConnectHeaders). This handler turns them into claims, and CYarp itself
    /// stores the resulting <see cref="ClaimsPrincipal"/> on
    /// <see cref="CYarp.Server.IClient.User"/> - which is why the upstream credential can be
    /// delivered without extending the CYarp API.
    /// </para>
    /// <para>
    /// Deliberately permissive: the handshake is never rejected. Any client that can reach this
    /// server may register a tunnel (and therefore claim any workspace id), which is an accepted
    /// trade-off for this deployment; see the security section of the README before exposing the
    /// server more widely.
    /// </para>
    /// </summary>
    internal sealed class CYarpTunnelAuthenticationHandler(
        IOptionsMonitor<AuthenticationSchemeOptions> options,
        ILoggerFactory logger,
        UrlEncoder encoder) : AuthenticationHandler<AuthenticationSchemeOptions>(options, logger, encoder)
    {
        public const string SchemeName = "CYarpTunnel";

        /// <summary>
        /// Header the CYarp client sends with the Uri it tunnels to. Only a CYarp handshake
        /// carries it, so it distinguishes a tunnel handshake from a public API call.
        /// </summary>
        private const string TargetUriHeader = "CYarp-TargetUri";

        protected override Task<AuthenticateResult> HandleAuthenticateAsync()
        {
            var targetUri = Request.Headers[TargetUriHeader].ToString();
            if (string.IsNullOrEmpty(targetUri))
            {
                // A public API call: leave it anonymous, the ApiKey middleware handles it.
                return Task.FromResult(AuthenticateResult.NoResult());
            }

            var clientId = Request.Headers[CYarpServerHost.ClientIdHeader].ToString();
            if (string.IsNullOrEmpty(clientId))
            {
                clientId = CYarpServerHost.DefaultClientId;
            }

            var claims = new List<Claim>(3)
            {
                new(CnbClaims.ClientId, clientId),
                new(CnbClaims.TargetUri, targetUri),
            };

            var upstreamAuthorization = Request.Headers.Authorization.ToString();
            if (string.IsNullOrEmpty(upstreamAuthorization) == false)
            {
                claims.Add(new Claim(CnbClaims.UpstreamAuthorization, upstreamAuthorization));
            }

            var identity = new ClaimsIdentity(claims, SchemeName);
            var ticket = new AuthenticationTicket(new ClaimsPrincipal(identity), SchemeName);
            return Task.FromResult(AuthenticateResult.Success(ticket));
        }
    }
}
