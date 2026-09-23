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
}
