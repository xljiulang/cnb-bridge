using System.Text.Json.Serialization;

namespace CYarpServer
{
    /// <summary>
    /// Response of the locally served /v1/models endpoint.
    /// </summary>
    public sealed class ModelListResponse
    {
        public string Object { get; set; } = "list";

        public ModelItem[] Data { get; set; } = [];
    }

    /// <summary>
    /// A single entry of <see cref="ModelListResponse"/>.
    /// </summary>
    public sealed class ModelItem
    {
        public required string Id { get; set; }

        public string Object { get; set; } = "model";

        public long Created { get; set; }

        [JsonPropertyName("owned_by")]
        public required string OwnedBy { get; set; }
    }

    /// <summary>
    /// Source generated JSON metadata. Required for Native AOT: reflection based
    /// serialization is not available at runtime.
    /// </summary>
    [JsonSourceGenerationOptions(PropertyNamingPolicy = JsonKnownNamingPolicy.CamelCase)]
    [JsonSerializable(typeof(ModelListResponse))]
    [JsonSerializable(typeof(string[]))]
    internal sealed partial class CnbJsonContext : JsonSerializerContext
    {
    }
}
