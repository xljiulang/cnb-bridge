using Microsoft.Extensions.Configuration;
using System;
using System.Globalization;

namespace CYarp.Server.Configs
{
    /// <summary>
    /// Binds <see cref="CYarpOptions"/> without the reflection based configuration
    /// binder.
    /// <para>
    /// CNB-CN delta: the reflection binder (and its source generated replacement) cannot
    /// handle <see cref="HttpTunnelConfig.ActivityHeadersPropagator"/>, and the generated
    /// binder then silently skips the whole HttpTunnel section. Binding the handful of
    /// configuration backed properties explicitly keeps every property working and keeps
    /// the Native AOT publish diagnostic free.
    /// </para>
    /// </summary>
    internal static class CYarpOptionsBinder
    {
        public static void Bind(CYarpOptions options, IConfiguration section)
        {
            options.Node = GetString(section, nameof(CYarpOptions.Node)) ?? options.Node;

            if (GetEnum(section, nameof(CYarpOptions.Protocols), out Protocols protocols))
            {
                options.Protocols = protocols;
            }

            var client = section.GetSection(nameof(CYarpOptions.Client));
            if (GetBool(client, nameof(ClientConfig.KeepAlive), out var keepAlive))
            {
                options.Client.KeepAlive = keepAlive;
            }
            if (GetTimeSpan(client, nameof(ClientConfig.KeepAliveInterval), out var keepAliveInterval))
            {
                options.Client.KeepAliveInterval = keepAliveInterval;
            }

            var httpTunnel = section.GetSection(nameof(CYarpOptions.HttpTunnel));
            if (GetTimeSpan(httpTunnel, nameof(HttpTunnelConfig.LifeTime), out var lifeTime))
            {
                options.HttpTunnel.LifeTime = lifeTime;
            }
            if (GetTimeSpan(httpTunnel, nameof(HttpTunnelConfig.IdleTimeout), out var idleTimeout))
            {
                options.HttpTunnel.IdleTimeout = idleTimeout;
            }
            if (GetInt32(httpTunnel, nameof(HttpTunnelConfig.MaxTunnelsPerClient), out var maxTunnelsPerClient))
            {
                options.HttpTunnel.MaxTunnelsPerClient = maxTunnelsPerClient;
            }
            if (GetTimeSpan(httpTunnel, nameof(HttpTunnelConfig.CreationTimeout), out var creationTimeout))
            {
                options.HttpTunnel.CreationTimeout = creationTimeout;
            }
            if (GetBool(httpTunnel, nameof(HttpTunnelConfig.DangerousAcceptAnyServerCertificate), out var acceptAnyCertificate))
            {
                options.HttpTunnel.DangerousAcceptAnyServerCertificate = acceptAnyCertificate;
            }
        }

        private static string? GetString(IConfiguration section, string key)
        {
            var value = section[key];
            return string.IsNullOrWhiteSpace(value) ? null : value;
        }

        private static bool GetBool(IConfiguration section, string key, out bool value)
        {
            return bool.TryParse(section[key], out value);
        }

        private static bool GetInt32(IConfiguration section, string key, out int value)
        {
            return int.TryParse(section[key], NumberStyles.Integer, CultureInfo.InvariantCulture, out value);
        }

        private static bool GetTimeSpan(IConfiguration section, string key, out TimeSpan value)
        {
            return TimeSpan.TryParse(section[key], CultureInfo.InvariantCulture, out value);
        }

        private static bool GetEnum<TEnum>(IConfiguration section, string key, out TEnum value) where TEnum : struct, Enum
        {
            return Enum.TryParse(section[key], ignoreCase: true, out value);
        }
    }
}
