using Microsoft.AspNetCore.Builder;
using Microsoft.AspNetCore.Hosting;
using Microsoft.AspNetCore.Http;
using Microsoft.AspNetCore.Hosting.Server;
using Microsoft.AspNetCore.Hosting.Server.Features;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Logging;
using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Stands in for the CNB AI endpoint that a CYarp client tunnels to.
    /// Records what actually arrived, so tests can prove the path rewriting and the
    /// credential replacement happened on the server side.
    /// </summary>
    internal sealed class StubUpstream : IAsyncDisposable
    {
        private readonly WebApplication app;
        private readonly List<RecordedRequest> requests = [];
        private readonly TaskCompletionSource streamGate = new(TaskCreationOptions.RunContinuationsAsynchronously);

        public StubUpstream()
        {
            var builder = WebApplication.CreateBuilder();
            builder.Logging.ClearProviders();
            builder.WebHost.UseUrls("http://127.0.0.1:0");

            this.app = builder.Build();
            this.app.Map("/{**catchall}", (RequestDelegate)this.HandleAsync);
        }

        public string BaseAddress { get; private set; } = string.Empty;

        public IReadOnlyList<RecordedRequest> Requests
        {
            get
            {
                lock (this.requests)
                {
                    return this.requests.ToArray();
                }
            }
        }

        public async Task StartAsync()
        {
            await this.app.StartAsync();
            this.BaseAddress = Address(this.app);
        }

        /// <summary>
        /// Lets a gated streaming response continue after the test observed its first frame.
        /// </summary>
        public void ReleaseStream()
        {
            this.streamGate.TrySetResult();
        }

        private async Task HandleAsync(HttpContext context)
        {
            string body;
            using (var reader = new StreamReader(context.Request.Body))
            {
                body = await reader.ReadToEndAsync();
            }

            var headers = context.Request.Headers.ToDictionary(header => header.Key, header => header.Value.ToString(), StringComparer.OrdinalIgnoreCase);
            lock (this.requests)
            {
                this.requests.Add(new RecordedRequest(
                    context.Request.Method,
                    context.Request.Path.Value ?? "/",
                    context.Request.QueryString.Value ?? string.Empty,
                    context.Request.Headers.Authorization.ToString(),
                    headers,
                    body));
            }

            context.Response.ContentType = "application/json";

            if (body.Contains("\"stream\":true", StringComparison.Ordinal))
            {
                // First frame, then block until the test has seen it: a proxy that buffers
                // the whole response can never deliver it.
                await context.Response.WriteAsync("data: {\"chunk\":1}\n\n");
                await context.Response.Body.FlushAsync();
                await this.streamGate.Task;
                await context.Response.WriteAsync("data: {\"chunk\":2}\n\n");
                await context.Response.Body.FlushAsync();
                return;
            }

            await context.Response.WriteAsync("""{"id":"stub-chatcmpl","object":"chat.completion","model":"model-a"}""");
        }

        public async ValueTask DisposeAsync()
        {
            this.streamGate.TrySetResult();
            await this.app.StopAsync();
            await this.app.DisposeAsync();
        }

        internal static string Address(WebApplication app)
        {
            return app.Services.GetRequiredService<IServer>().Features.Get<IServerAddressesFeature>()!.Addresses.First();
        }

        internal sealed record RecordedRequest(
            string Method,
            string Path,
            string Query,
            string Authorization,
            IReadOnlyDictionary<string, string> Headers,
            string Body);
    }
}
