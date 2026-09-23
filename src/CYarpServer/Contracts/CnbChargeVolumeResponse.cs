using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// <c>GET /{org}/-/charge/volume</c> 响应里本服务端读取的部分：该组织当月已消耗的
    /// credit，以及为在途任务预冻结的 credit。
    /// </summary>
    internal sealed class CnbChargeVolumeResponse
    {
        /// <summary>
        /// 获取或设置已消耗的 credit（单位 milli-credit）。
        /// </summary>
        [JsonPropertyName("credit_in_milli")]
        public long? CreditInMilli { get; set; }

        /// <summary>
        /// 获取或设置预冻结的 credit（单位 milli-credit）；缺失时按 0 处理。
        /// </summary>
        [JsonPropertyName("freeze_credit_in_milli")]
        public long? FreezeCreditInMilli { get; set; }
    }
}
