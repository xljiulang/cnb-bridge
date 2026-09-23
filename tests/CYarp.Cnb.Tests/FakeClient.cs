using CYarp.Server;
using System;
using System.IO;
using System.Net;
using System.Security.Claims;
using System.Threading;
using System.Threading.Tasks;
using Yarp.ReverseProxy.Forwarder;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// An <see cref="IClient"/> that only carries an id, for the routing tests: the routing
    /// decision reads nothing but the id and the probed quota.
    /// </summary>
    internal sealed class FakeClient(string id, Uri? targetUri = null) : IClient
    {
        private static readonly ClaimsPrincipal Principal = new(new ClaimsIdentity());

        public string Id { get; } = id;

        public Uri TargetUri { get; } = targetUri ?? new Uri("https://api.cnb.cool/org/repo/-/ai");

        public ClaimsPrincipal User => Principal;

        public TransportProtocol Protocol => TransportProtocol.Http11;

        public IPEndPoint? RemoteEndpoint => null;

        public int TcpTunnelCount => 0;

        public int HttpTunnelCount => 0;

        public DateTimeOffset CreationTime => DateTimeOffset.UnixEpoch;

        public Task<Stream> CreateTcpTunnelAsync(CancellationToken cancellationToken)
        {
            throw new NotSupportedException("The routing tests never open a tunnel.");
        }

        public ValueTask<ForwarderError> ForwardHttpAsync(Microsoft.AspNetCore.Http.HttpContext context, HttpTransformer? transformer)
        {
            throw new NotSupportedException("The routing tests never forward a request.");
        }

        public ValueTask DisposeAsync()
        {
            return ValueTask.CompletedTask;
        }

        public override string ToString()
        {
            return this.Id;
        }
    }
}
