using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// <c>GET /{org}/-/charge/quota</c> 响应里本服务端读取的部分：组织的当月额度。
    /// 响应里其它字段都是刻意忽略的。
    /// </summary>
    internal sealed class CnbChargeQuotaResponse
    {
        /// <summary>
        /// 获取或设置 credit 额度项（含总额度与免费额度，单位 milli-credit）。
        /// </summary>
        [JsonPropertyName("credit_in_milli")]
        public CnbQuotaItem? CreditInMilli { get; set; }
    }
}
