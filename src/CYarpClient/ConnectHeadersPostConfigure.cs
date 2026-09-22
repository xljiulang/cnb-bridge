using CYarp.Client;
using Microsoft.Extensions.Configuration;
using Microsoft.Extensions.Options;

namespace CYarpClient
{
    /// <summary>
    /// Fills <see cref="CYarpClientOptions.ConnectHeaders"/> from configuration.
    /// The dictionary cannot be bound by the configuration binding source generator,
    /// so it is applied here to keep the Native AOT build correct.
    /// </summary>
    internal sealed class ConnectHeadersPostConfigure(IConfiguration configuration) : IPostConfigureOptions<CYarpClientOptions>
    {
        public void PostConfigure(string? name, CYarpClientOptions options)
        {
            var section = configuration.GetSection($"{nameof(CYarpClientOptions)}:ConnectHeaders");
            foreach (var child in section.GetChildren())
            {
                options.ConnectHeaders[child.Key] = child.Value ?? string.Empty;
            }
        }
    }
}
