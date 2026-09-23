using System.Text.Json.Serialization;

namespace CYarpServer
{
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
}
