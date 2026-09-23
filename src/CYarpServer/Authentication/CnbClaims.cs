namespace CYarpServer.Authentication
{
    /// <summary>
    /// Claims carried by the CYarp tunnel handshake principal. CYarp stores the
    /// <see cref="System.Security.Claims.ClaimsPrincipal"/> of the handshake request on
    /// <see cref="CYarp.Server.IClient.User"/>, so this is the channel that carries the
    /// workspace identity and its upstream credential to the forwarding code.
    /// </summary>
    public static class CnbClaims
    {
        /// <summary>
        /// The workspace id (CNB repo slug with "/" replaced by "~").
        /// </summary>
        public const string ClientId = "cnb:client-id";

        /// <summary>
        /// The Authorization header the client reported, e.g. "Bearer &lt;cnb-token&gt;".
        /// The server replays it to the upstream so that CNB can authenticate the call.
        /// </summary>
        public const string UpstreamAuthorization = "cnb:upstream-authorization";

        /// <summary>
        /// The upstream base Uri the client tunnels to (diagnostics only).
        /// </summary>
        public const string TargetUri = "cnb:target-uri";
    }
}
