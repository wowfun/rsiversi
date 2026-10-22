# 故障排查

[English](../../../user/troubleshooting.md)

本页涵盖源码安装和本地 Web/TUI/Desktop。先查看启动终端中的完整错误。
反馈问题时提供系统、Shell、源码版本和执行命令，
并移除 API 密钥、启动票据和设备令牌。

| 现象 | 下一步 |
| --- | --- |
| 缺少可选包 | 使用 `rsi plugin install NAME` 或显式本地包安装，详见[安装说明](source-install.md)。 |
| 缺少源码构建依赖 | 按[安装说明](source-install.md)安装对应产物的构建依赖，再次运行构建。 |
| PowerShell 拒绝脚本 | 使用[安装说明](source-install.md)中的进程级命令；组织策略可能需要管理员处理。 |
| Node 或 pnpm 版本不对 | 构建 Web 前选择 Web 包及锁文件声明的版本。 |
| 路径过长 | 把 Git 仓库放到本地 NTFS 的较短路径，检查 Git 长路径配置与系统策略。脚本不会修改它们。 |
| 构建失败 | 查看构建工具输出或源码构建的有界诊断。watch 构建失败会保留上一份已准入的包。 |
| 端口占用或访问被拒绝 | 停止之前的实例，或给 Web 传入 `--port 0` 自动选择空闲端口，再使用新打印的地址。 |
| 浏览器没有打开 | 复制完整启动链接，也可以使用 `--no-open`。首次登录不能只复制域名和端口。 |
| 登录链接失效 | 再次运行 `rsi web` 获取新链接。票据只能使用一次，十分钟后过期，详见 [Web 启动契约](../../../../apps/serve/README.md)。 |
| 配置界面只读 | 通过当前本机启动链接重新进入；远程设备需要明确的配置授权。 |
| 没有默认模型 | 分别保存凭据、服务商和默认模型，然后新建对话。 |
| 认证被拒绝 | 检查服务商、凭据槽位和账号密钥；本地存在凭据不等于远程验证成功。 |
| 未知模型或协议错误 | 向服务商核对准确的模型标识、接口地址和协议。 |
| Host 已被占用 | 普通应用共享常驻 Host；维护前使用相同 home 选项运行 `rsi host stop`。不要删除所有者锁，详见 [Host 所有权](../../../../crates/rsi/service-host/README.md)。 |
| TUI 拒绝 mintty/conhost | 在 Windows Terminal 中打开 PowerShell 或 Git Bash，不要套用 winpty，详见[终端要求](../../../../apps/terminal/README.md)。 |
| 工作目录被拒绝 | Windows 工作区和私有状态要求固定本地 NTFS，不支持网络路径、ReFS Dev Drive 或 exFAT。详见[平台支持](../../../../crates/rsi/README.md)。 |

## Windows 能力限制

Web、TUI、Desktop 和 ACP 共享常驻 [Service Host](../../../../crates/rsi/core/README.md)。
Web 目录浏览器暂不可用，请手工输入工作目录。

受限 Shell 执行暂不可用。AppContainer 可行性探针与已发布的沙箱实现分开，
全权限执行不能代替沙箱验收。工作区审阅需要更强的源码读取隔离，
因此在 Windows 上暂不可用。

Linux SSH 目标、Web PTY、Browser/Automation、
已完成进程的输出缓存和 MCP Profile 管理也不在本轮 Windows 支持范围内。
界面上的部分操作可能返回明确的不可用提示。
需要这些 Linux 工作流时请使用 Linux/WSL，不要把 Windows 构建成功理解为全部能力都已对齐。
