# xsoc 文档

本文档集描述当前 `1.0.0` CLI 与系统服务。命令行为以当前源码、`xsoc --help` 和结构化输出为准；
`releases/` 中的版本说明只记录对应历史版本，不能替代当前操作手册。

| 文档 | 内容 |
|---|---|
| [../README.md](../README.md) | GitHub 首页简介、最短配置路径和开发验证 |
| [configuration.md](configuration.md) | 初始化、候选配置、配对/恢复、服务启动和诊断的完整命令 |
| [platform-setup.md](platform-setup.md) | Windows、Linux、macOS 安装、配对/恢复、服务查看/启停、自启、诊断、升级与卸载，含命令解释 |
| [releases/1.0.0.md](releases/1.0.0.md) | 当前 CLI、状态格式、平台和升级/回退边界 |
| [releases/](releases/) | 历史发行记录 |

客户端只读采集主机遥测并主动连接服务端，不含托盘、本地网页或浏览器入口。`probe` 不联网；`once` 与
`doctor --delivery` 会产生真实投递；`doctor --network` 只访问公开健康端点，不能证明服务账户或报告凭据可用。

CLI 参数、输出与兼容性约定见 [CLI 兼容性](cli-compatibility.md)。

公共支撑的职责、单体依赖、平台边界与验证方法见[公共支撑说明](common-support.md)。
