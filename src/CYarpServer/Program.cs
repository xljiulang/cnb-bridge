using Microsoft.Extensions.Hosting;
using ServiceSelf;
using System;

namespace CYarpServer
{
    /// <summary>
    /// CYarpServer process entry point.
    /// <para>
    /// ServiceSelf turns this single binary into a system service, so the server is
    /// deployed without Docker or a reverse proxy:
    /// <c>sudo ./CYarpServer start</c> installs and starts it (systemd on Linux, SCM on
    /// Windows), <c>./CYarpServer stop</c> stops and removes it, <c>./CYarpServer logs</c>
    /// streams its log.
    /// </para>
    /// </summary>
    public static class Program
    {
        private const string ServiceName = "CYarpServer";

        public static void Main(string[] args)
        {
            // Returns false when a service command (start/stop/logs) was handled.
            if (Service.UseServiceSelf(args, ServiceName, CreateServiceOptions()) == false)
            {
                return;
            }

            var app = CYarpServerHost.Create(args, builder => builder.Host.UseServiceSelf());
            app.Run();
        }

        private static ServiceOptions CreateServiceOptions()
        {
            var options = new ServiceOptions
            {
                Description = "Exposes the CNB workspace AI endpoint as a fixed domain OpenAI-compatible API over a CYarp tunnel.",
            };

            // Keep the tunnel up, and start from the directory that holds appsettings.json.
            options.Linux.Service.Restart = "always";
            options.Linux.Service.RestartSec = "5";
            options.Linux.Service.WorkingDirectory = AppContext.BaseDirectory;

            options.Windows.FailureActionType = WindowsServiceActionType.Restart;

            return options;
        }
    }
}
