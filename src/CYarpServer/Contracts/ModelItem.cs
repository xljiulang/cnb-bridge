using System.Text.Json.Serialization;

namespace CYarpServer.Contracts
{
    /// <summary>
    /// <see cref="ModelListResponse"/> 中的单个模型条目。
    /// </summary>
    public sealed class ModelItem
    {
        /// <summary>
        /// 获取或设置模型 id，取自 ProxyModels 配置。
        /// </summary>
        public required string Id { get; set; }

        /// <summary>
        /// 获取或设置对象类型，恒为 "model"。
        /// </summary>
        public string Object { get; set; } = "model";

        /// <summary>
        /// 获取或设置创建时间；本服务端不维护模型元数据，恒为 0。
        /// </summary>
        public long Created { get; set; }

        /// <summary>
        /// 获取或设置模型归属方；本服务端固定为 "cnb"。
        /// </summary>
        [JsonPropertyName("owned_by")]
        public required string OwnedBy { get; set; }
    }
}
