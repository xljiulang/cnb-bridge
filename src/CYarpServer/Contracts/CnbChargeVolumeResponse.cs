using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// The parts of <c>GET /{org}/-/charge/volume</c> that this server reads: how many credits
    /// the organization already spent this month and how many are frozen for in-flight tasks.
    /// </summary>
    internal sealed class CnbChargeVolumeResponse
    {
        [JsonPropertyName("credit_in_milli")]
        public long? CreditInMilli { get; set; }

        [JsonPropertyName("freeze_credit_in_milli")]
        public long? FreezeCreditInMilli { get; set; }
    }
}
