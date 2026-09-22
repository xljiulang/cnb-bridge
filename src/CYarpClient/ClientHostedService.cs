using CYarp.Client;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Microsoft.Extensions.Options;
using System;
using System.Threading;
using System.Threading.Tasks;

namespace CYarpClient
{
    /// <summary>
    /// Keeps the tunnel to the CYarpServer alive, reconnecting forever.
    /// </summary>
    internal sealed class ClientHostedService(
        IOptionsMonitor<CYarpClientOptions> clientOptions,
        ILogger<CYarp.Client.CYarpClient> logger) : BackgroundService
    {
        protected override async Task ExecuteAsync(CancellationToken stoppingToken)
        {
            while (stoppingToken.IsCancellationRequested == false)
            {
                try
                {
                    var options = clientOptions.CurrentValue;
                    using var client = new CYarp.Client.CYarpClient(options, logger);
                    await client.TransportAsync(stoppingToken);

                    logger.LogInformation("Transport has been closed, will reconnect after 5 seconds");
                    await Task.Delay(TimeSpan.FromSeconds(5d), stoppingToken);
                }
                catch (ArgumentException)
                {
                    // Invalid configuration: no point in retrying.
                    throw;
                }
                catch (OperationCanceledException) when (stoppingToken.IsCancellationRequested)
                {
                    break;
                }
                catch (CYarpConnectException ex) when (ex.ErrorCode >= CYarpConnectError.Unauthorized)
                {
                    logger.LogError(ex, "The server rejected the connection, giving up");
                    break;
                }
                catch (Exception ex)
                {
                    logger.LogError(ex, "Connection error, will reconnect after 10 seconds");
                    await Task.Delay(TimeSpan.FromSeconds(10d), stoppingToken);
                }
            }
        }
    }
}
