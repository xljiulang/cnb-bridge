using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// Source generated JSON metadata. Required for Native AOT: reflection based
    /// serialization is not available at runtime.
    /// </summary>
    [JsonSourceGenerationOptions(
        PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase,
        // Keep null members (e.g. the "error" of a healthy workspace) instead of dropping them,
        // so every object of a response has the same shape.
        DefaultIgnoreCondition = JsonIgnoreCondition.Never)]
    [JsonSerializable(typeof(ModelListResponse))]
    [JsonSerializable(typeof(ClientInfo[]))]
    [JsonSerializable(typeof(CnbChargeQuotaResponse))]
    [JsonSerializable(typeof(CnbChargeVolumeResponse))]
    internal sealed partial class CnbJsonContext : JsonSerializerContext
    {
    }
}
