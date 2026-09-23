using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// 源生成的 JSON 元数据。Native AOT 下必需：运行时没有基于反射的序列化可用。
    /// </summary>
    [JsonSourceGenerationOptions(
        PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase,
        // 保留为 null 的成员（例如健康工作空间的 "error"）而不是丢弃它们，
        // 这样一条响应里每个对象的字段形状都一致。
        DefaultIgnoreCondition = JsonIgnoreCondition.Never)]
    [JsonSerializable(typeof(ModelListResponse))]
    [JsonSerializable(typeof(ClientInfo[]))]
    [JsonSerializable(typeof(CnbChargeQuotaResponse))]
    [JsonSerializable(typeof(CnbChargeVolumeResponse))]
    internal sealed partial class CnbJsonContext : JsonSerializerContext
    {
    }
}
