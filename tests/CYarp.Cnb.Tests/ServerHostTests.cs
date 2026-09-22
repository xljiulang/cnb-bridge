using CYarpServer;
using System.Collections.Generic;
using System.Threading.Tasks;
using Xunit;

namespace CYarp.Cnb.Tests
{
    /// <summary>
    /// Host level checks that do not need a workspace.
    /// </summary>
    public sealed class ServerHostTests
    {
        [Fact]
        public async Task ListenAddress_IsTakenFromConfiguration()
        {
            // Guards the shadowing bug: a hard coded "Urls" entry in appsettings.json used to
            // win over the configured address and made the server bind :80 instead.
            await using var server = CYarpServerHost.Create(
                [],
                settings: new Dictionary<string, string?> { ["Urls"] = "http://127.0.0.1:0" });

            await server.StartAsync();
            var address = StubUpstream.Address(server);
            await server.StopAsync();

            Assert.StartsWith("http://127.0.0.1:", address);
        }

        [Fact]
        public async Task ProxyKeyUnset_AllowsAnonymousAccess()
        {
            await using var server = CYarpServerHost.Create(
                [],
                settings: new Dictionary<string, string?>
                {
                    ["Urls"] = "http://127.0.0.1:0",
                    ["ProxyKey"] = string.Empty,
                    ["ProxyModels"] = "model-a",
                    ["Logging:LogLevel:Default"] = "Warning",
                });

            await server.StartAsync();

            using var client = new System.Net.Http.HttpClient { BaseAddress = new System.Uri(StubUpstream.Address(server)) };
            using var response = await client.GetAsync("/v1/models");

            Assert.Equal(System.Net.HttpStatusCode.OK, response.StatusCode);
            Assert.Contains("\"id\":\"model-a\"", await response.Content.ReadAsStringAsync());

            await server.StopAsync();
        }
    }
}
