using Microsoft.Extensions.Hosting;
using ServiceSelf;
using System;

namespace CYarpServer
{
    /// <summary>
    /// CYarpServer 进程入口。
    /// <para>
    /// ServiceSelf 把这个单文件变成系统服务，因此服务端不依赖 Docker、也不需要反向代理：
    /// <c>sudo ./CYarpServer start</c> 安装并启动（Linux 用 systemd，Windows 用 SCM），
    /// <c>./CYarpServer stop</c> 停止并删除，<c>./CYarpServer logs</c> 订阅它的日志。
    /// </para>
    /// </summary>
    public static class Program
    {
        private const string ServiceName = "CYarpServer";

        /// <summary>
        /// 进程主入口：先让 ServiceSelf 处理服务子命令（start/stop/logs），否则前台运行 Web 应用。
        /// </summary>
        /// <param name="args">命令行参数。</param>
        public static void Main(string[] args)
        {
            // 处理了服务子命令（start/stop/logs）时返回 false。
            if (Service.UseServiceSelf(args, ServiceName, CreateServiceOptions()) == false)
            {
                return;
            }

            var app = CYarpServerHost.Create(args, builder => builder.Host.UseServiceSelf());
            app.Run();
        }

        /// <summary>
        /// 构造 ServiceSelf 的服务选项（描述与重启策略）。
        /// </summary>
        /// <returns>服务选项。</returns>
        private static ServiceOptions CreateServiceOptions()
        {
            var options = new ServiceOptions
            {
                Description = "Exposes the CNB workspace AI endpoint as a fixed domain OpenAI-compatible API over a CYarp tunnel.",
            };

            // 保持隧道常在线，并从存放 appsettings.json 的目录启动。
            options.Linux.Service.Restart = "always";
            options.Linux.Service.RestartSec = "5";
            options.Linux.Service.WorkingDirectory = AppContext.BaseDirectory;

            options.Windows.FailureActionType = WindowsServiceActionType.Restart;

            return options;
        }
    }
}
