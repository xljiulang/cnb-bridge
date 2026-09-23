namespace CYarpServer.Contracts
{
    /// <summary>
    /// 本地应答的 /v1/models 端点的响应。
    /// </summary>
    public sealed class ModelListResponse
    {
        /// <summary>
        /// 获取或设置对象类型，恒为 "list"。
        /// </summary>
        public string Object { get; set; } = "list";

        /// <summary>
        /// 获取或设置模型条目列表。
        /// </summary>
        public ModelItem[] Data { get; set; } = [];
    }
}
