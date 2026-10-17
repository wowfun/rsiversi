# 使用终端界面

[English](terminal.md)

TUI 提供全屏对话和模型配置。请先[从源码安装](source-install.zh.md)。
Windows 上需要在 **Windows Terminal** 中运行 PowerShell 或 Git Bash，
mintty 和经典 conhost 控制台暂不支持此 TUI。
使用同一组 Host 数据目录前，请先停止 Web。

<!-- rsi-example id=tui-windows platform=windows -->
```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File .\rsi.ps1 tui
```

<!-- rsi-example id=tui-bash platform=git-bash -->
```bash
bash rsi.sh tui
```

尚未配置模型时会打开首页或设置界面。
使用 `/login deepseek` 配置凭据，`/model` 选择模型，
`/help` 查看命令。`/new` 新建对话，`/resume` 打开历史对话。

终端的括号粘贴模式会把多行内容作为一份草稿插入，发送前请检查。
不同操作的快捷键见 [TUI 交互参考](../../../../apps/terminal/docs/tui-design.md)。
使用 `/quit` 或 `/exit` 退出。在首页按 Ctrl+C 也会退出，即使输入框已有草稿。
退出时未发送的草稿会丢弃。

终端宿主正常响应时，Windows 终端实现会在正常退出和已处理的取消后恢复控制台模式与光标。
清理保证由[终端所有权契约](../../../../apps/terminal/README.md)定义。
强制杀死进程或终端宿主无响应时无法保证完整清理；如果之后显示异常，请重新打开终端标签页。

