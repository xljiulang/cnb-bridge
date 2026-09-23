using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// CNB charge API 的额度项：免费额度与总额度（免费 + 付费 + 特权）。
    /// </summary>
    internal sealed class CnbQuotaItem
    {
        /// <summary>
        /// 获取或设置总额度（含免费、付费与特权额度），单位 milli-credit。
        /// 路由用的是它，而不是只含免费的 <see cref="Free"/>。
        /// </summary>
        [JsonPropertyName("total")]
        public long? Total { get; set; }

        /// <summary>
        /// 获取或设置免费额度，单位 milli-credit。仅用于诊断展示，不参与选路。
        /// </summary>
        [JsonPropertyName("free")]
        public long? Free { get; set; }
    }
}
