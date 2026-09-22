using CYarp.Client;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.IO;

namespace CYarpClient
{
    /// <summary>
    /// CYarpClient process entry point: a long lived tunnel from inside the CNB
    /// workspace to the public CYarpServer.
    /// <para>
    /// Every setting comes from configuration, and environment variables (with the
    /// <c>__</c> nesting separator) override appsettings.json, so the CNB token only
    /// ever lives in the environment:
    /// <c>CYarpClientOptions__ConnectHeaders__Authorization="Bearer $CNB_TOKEN"</c>.
    /// </para>
    /// </summary>
    public static class Program
    {
        public static void Main(string[] args)
        {
            var host = Host.CreateDefaultBuilder(args)
                // appsettings.json lives next to the binary, also when started from another cwd.
                .UseContentRoot(AppContext.BaseDirectory)
                .ConfigureServices((context, services) =>
                {
                    services.AddLogging(builder => builder.AddProvider(new FileLoggerProvider(Path.Combine(AppContext.BaseDirectory, "logs"))));

                    services.Configure<CYarpClientOptions>(context.Configuration.GetSection(nameof(CYarpClientOptions)));

                    // The handshake headers are a dictionary, which the source generated
                    // binder does not cover: filled in explicitly so the CNB token is
                    // always delivered, also under Native AOT.
                    services.AddSingleton<IPostConfigureOptions<CYarpClientOptions>, ConnectHeadersPostConfigure>();

                    services.AddHostedService<ClientHostedService>();
                })
                .Build();

            host.Run();
        }
    }
}
