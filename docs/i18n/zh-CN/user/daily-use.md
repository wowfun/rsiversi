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

Windows 和 Linux 上使用同一组配置、状态目录的应用共享常驻 Host。
TUI、Web 和 Desktop 可以同时运行。关闭界面只断开该客户端，
不会停止 Host，也不会取消其他客户端的回复。
退出终端界面时请按 Ctrl+C，并等待清理完成。

`rsi web` 打开新的启动链接后返回。`rsi web status` 查看浏览器 listener，
`rsi web stop` 停止该 listener。`rsi host stop` 停止共享 Host，
并断开它的所有客户端。
详见 [Host 所有权契约](../../../../crates/rsi/core/README.md)。

设备管理同样连接共享 Host。例如在 PowerShell 中执行：

<!-- rsi-example id=devices-windows platform=windows -->
```powershell
rsi run devices -- list
```

随后把列出的设备 ID 传给 `rsi run devices -- revoke DEVICE_ID`。
不要把设备注册回执贴到公开问题中，它可能包含令牌。

## 查找数据

Windows 使用标准的用户应用配置、状态和缓存位置下的 `rsi` 目录，
Linux 使用对应的 XDG 位置。
显式设置的 `XDG_CONFIG_HOME`、`XDG_STATE_HOME` 和 `XDG_CACHE_HOME` 必须是绝对路径。
两个平台都会在这些位置下追加 `rsi` 子目录。
`--config-home`、`--state-home` 和 `--cache-home` 直接选择最终目录，
不追加 `rsi`；相对路径按调用者工作目录解析。
恢复历史时要使用同一组位置。备份配置和状态前先停止 Host，并妥善保管凭据。
不要为修复构建失败而删除这些目录，构建产物位于仓库的 `target` 下。

精确的路径解析规则由 [Host 路径契约](../../../../crates/rsi/core/README.md)定义。
