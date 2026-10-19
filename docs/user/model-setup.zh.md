# 配置模型

[English](model-setup.md)

本页说明 Web 的 **Settings → Models & access（设置 → 模型与访问权限）**。
每个区域分别保存。通过本机启动链接进入，浏览器才会获得配置权限。
如果页面显示只读，见[故障排查](troubleshooting.zh.md)。

1. 在 **Credential（凭据）** 中选择服务商。首次配置可保留
   **Credential slot（凭据槽位）** 为 `default`。
   把密钥粘贴到 **API key**，点击 **Save credential（保存凭据）**。
   **Check credential（检查凭据）** 只检查本地是否保存了凭据，
   不代表远程服务商已经接受它。
2. 在 **Add provider（添加服务商）** 中填写唯一的
   **Deployment name（部署名称）**，例如 `my-deepseek`。
   这是你为这条连接起的本地名称。填写服务商的 **Endpoint（接口地址）**、
   协议和准确的 **Model identifier（模型标识）**。
   在 **Context and output limits（上下文与输出限制）** 中按服务商提供的值填写，
   点击 **Apply provider（应用服务商）**，等待模型出现在 **Default model（默认模型）** 中。
3. 在 **Conversation defaults（对话默认设置）** 中选择这个部署及模型。
   选择后会自动保存，请检查 **Operation results（操作结果）** 是否有错误。
   新对话使用保存的默认值，已有对话保留创建时的设置。

DeepSeek 的接口地址通常以 `https://api.deepseek.com` 开头。
账号和模型使用 Chat Completions API 时选择 **Chat completions**；
只有部署支持 Responses API 时才选择 **Responses**。
不要根据显示名称猜测模型标识。这里的模型列表来自保存的部署配置，
不会自动查询账号的所有模型。

应用服务商前，展开 **Context and output limits（上下文与输出限制）**。
预填数字只是示例，不是自动查询到的模型容量。`context window tokens` 是总上下文容量，
`max output reserve tokens` 是模型允许的最大输出容量。对于 DeepSeek，这个上限还须容纳
配置的[默认请求输出量](../../crates/rsi-ai/deepseek/README.md#generation-policy)。
调低 `default output reserve tokens` 不会改变该请求默认值。
如果 **Apply provider** 提示 `invalid API input`，先核对这些容量，再检查密钥；
这一步尚未向服务商发送推理请求。

OpenAI compatible（OpenAI 兼容）连接还需要 **Request path（请求路径）**，
常见值为 `/chat/completions`。只有选定模型支持图片时才启用图片输入。

凭据由原生凭据组件保存在 RSIversi 私有配置目录中，保存后的设置视图不会返回密钥。
应用不会自动导入仓库的 `.env` 文件。不要把密钥放入命令、截图或分享的日志中。

修改默认值后，新建对话并发送一条简短文本。
认证错误、未知模型和本地沙箱拒绝的原因不同，请按[故障排查](troubleshooting.zh.md)中的对应步骤处理。
