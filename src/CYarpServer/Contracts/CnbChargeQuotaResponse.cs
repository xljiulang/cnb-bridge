using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// The part of <c>GET /{org}/-/charge/quota</c> that this server reads: the organization's
    /// monthly credit allowance. Every other field of the response is ignored on purpose.
    /// </summary>
    internal sealed class CnbChargeQuotaResponse
    {
        [JsonPropertyName("credit_in_milli")]
        public CnbQuotaItem? CreditInMilli { get; set; }
    }
}
