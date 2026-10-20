# 日常使用

[English](../../../user/daily-use.md)

每个对话属于选定的工作区，并保存自己的历史记录。
选择已有对话可以继续交流，点击 **New conversation（新对话）** 会开始新的上下文。
修改默认模型只影响新对话。

使用停止按钮取消正在生成的回复，等状态稳定后再重试。
只关闭标签页不会取消工作。取消前可能已经发生文件修改，
重复任务前请先检查工作目录。

刷新页面和新开标签页会使用当前浏览器的 Cookie。
如果清除了 Cookie、撤销了浏览器设备，或丢失了未使用的启动链接，
请重新启动 Web 并使用新链接。启动链接中的票据有效期为十分钟，只能使用一次；
具体行为由 [Web 启动契约](../../../../apps/serve/README.md)定义。

## 停止与切换界面

在启动终端按 Ctrl+C，并等待退出。
Windows 的服务运行在这个进程内，Web 和 TUI 不能同时使用同一组 Host 数据目录，
切换前要先停止当前界面。强制关闭终端可能缩短清理时间，请尽量用 Ctrl+C。
详见 [Host 所有权契约](../../../../crates/rsi/core/README.md)。

Windows 上撤销浏览器设备前，先停止 Web，再在 PowerShell 中执行：

<!-- rsi-example id=devices-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -Command '& (Join-Path (& .\scripts\rsiversi.ps1 path web) rsi.exe) --profile devices list'
```

随后把列出的设备 ID 传给 `--profile devices revoke DEVICE_ID`。
如果执行策略限制脚本，请按[安装说明](source-install.md)
使用 `-ExecutionPolicy Bypass` 启动 PowerShell 进程。
不要把设备注册回执贴到公开问题中，它可能包含令牌。
Linux 的设备管理会连接已经运行的 Service。

## 查找数据

Windows 使用标准的用户应用配置、状态和缓存位置下的 `rsi` 目录，
Linux 使用对应的 XDG 位置。
显式设置的 `XDG_CONFIG_HOME`、`XDG_STATE_HOME` 和 `XDG_CACHE_HOME` 必须是绝对路径。
两个平台都会在这些位置下追加 `rsi` 子目录。
恢复历史时要使用同一组位置。备份配置和状态前先停止应用，并妥善保管凭据。
不要为修复构建失败而删除这些目录，构建产物位于仓库的 `target` 下。

精确的路径解析规则由 [Host 路径契约](../../../../crates/rsi/core/README.md)定义。
