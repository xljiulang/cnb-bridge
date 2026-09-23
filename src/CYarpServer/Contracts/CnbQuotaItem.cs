using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// A quota item of the CNB charge API: the free allowance and the total (free + paid +
    /// privilege).
    /// </summary>
    internal sealed class CnbQuotaItem
    {
        [JsonPropertyName("total")]
        public long? Total { get; set; }

        [JsonPropertyName("free")]
        public long? Free { get; set; }
    }
}
