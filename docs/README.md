# xsoc 使用文档

xsoc 是只读主机监控客户端，以系统服务主动连接 xsos。这里从安装一台主机开始，介绍当前 1.0.0 的使用和维护。

## 选择平台

- [Linux](platforms/linux.md)：x86_64 DEB / RPM，Ubuntu 24.04 构建与验证基线
- [Windows](platforms/windows.md)：Windows 11 x64 MSI
- [macOS](platforms/macos.md)：Apple Silicon arm64 PKG
- [Android 宿主库](platforms/android.md)：原生应用接入与 aarch64 目标检查
- [iOS / iPadOS 宿主库](platforms/ios.md)：真机与模拟器目标检查

各桌面平台页包含安装、构建和本机排障。首次安装需要的资料及通用向导输入见[安装入口](platform-setup.md)。

## 安装之后

1. [确认运行](usage.md)：核对服务、绑定及最新报告。
2. [调整配置](configuration.md)：修改设置或准备自动化输入。
3. [日常维护](administration.md)：服务管理和凭据更新。

## 按任务查找

- [日常维护](administration.md)：服务启停、自启、授权码更新和队列
- [排查问题](troubleshooting.md)：网络、认证与运行问题
- [命令参考](cli-compatibility.md)：命令、输出和状态路径
- [开发指南](development.md)：共用工具链、测试和源码结构
- [公共库](common-support.md)与[原生接口审查](unsafe-audit.md)
- [1.0.0 发行说明](releases/1.0.0.md)

指标范围与设备要求见[硬件监控](hardware-monitoring.md)和[Windows AMD / Intel GPU](windows-gpu-vendors.md)。
