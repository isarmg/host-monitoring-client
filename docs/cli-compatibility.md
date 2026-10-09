# xsoc 1.0.0 当前兼容边界

Client 支持 Windows x64、Linux x64 与 macOS Apple Silicon。部署时核对安装包版本、源码身份、Server 版本及配置状态。

| 维度 | 当前契约 |
| --- | --- |
| Client 程序 | 1.0.0 |
| Server | 报告 schema 1 |
| Client 配置与身份 | 只接受当前格式 1.0.0；软件发行号不改变该独立格式 |
| 本地队列 | 保留 Host UUID、报告 ID 和待发送报告的原有归属 |
| CLI JSON | schema_version 1；--format json 为机器输出入口 |
| Client Foundation | 0.10.5，固定提交 ab53bb6157117169b6d03d0a61497faa9ca718bd |
| Host 协议 crate | 1.0.0，固定提交 44e2090f113a78d38b26d59c638756de37c60383；schema 1 报告字段 |
| IPC | Foundation GetStatus/1，校验进程世代与绑定 |

安装器负责程序和服务的覆盖或修复，并按平台保留配置、身份与待发送队列；具体步骤见平台安装指南。配置或账户资料不兼容时，Client 返回明确错误；管理员须先验证队列，再通过受支持的配对恢复命令归档不兼容账户文件。未知或损坏的队列不能通过删除状态或重新配对绕过校验。

Server 数据库的备份或恢复流程不适用于 Client 状态。Client 不提供跨平台状态复制，也不承诺自动降级。

当前软件基线为 1.0.0，配套安装与诊断步骤见[分平台部署指南](platform-setup.md)。
