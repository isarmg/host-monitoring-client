# Host Monitoring Client 0.9.35 当前兼容边界

Client 支持 Windows x64、Linux x64 与 macOS Apple Silicon。部署时核对安装包版本、源码身份、Server 版本及配置状态。

| 维度 | 当前契约 |
| --- | --- |
| Client 程序 | 0.9.35 |
| Server | 0.9.31；报告 schema 3 |
| Client 配置与身份 | 写入格式 0.9.4；结构相同的 0.9.3 可读取 |
| 本地队列 | 保留 Host UUID、报告 ID 和待发送报告的原有归属 |
| CLI JSON | schema_version 1；--format json 为机器输出入口 |
| Client Foundation | 0.9.15，固定提交 8890ced415793b144997ebb04728f52fbe3e7e59 |
| Host 协议 crate | 0.9.30，固定提交 9c8facb7fc355dd7afe809bbaf2014e5560e0f35；与 Server 0.9.31 的 schema 3 报告字段一致 |
| IPC | Foundation GetStatus/1，校验进程世代与绑定 |

安装器负责程序和服务的覆盖或修复，并按平台保留配置、身份与待发送队列；具体步骤见平台安装指南。配置或账户资料不兼容时，Client 返回明确错误；管理员须先验证队列，再通过受支持的配对恢复命令归档不兼容账户文件。未知或损坏的队列不能通过删除状态或重新配对绕过校验。

Server 数据库的备份或恢复流程不适用于 Client 状态。Client 不提供跨平台状态复制，也不承诺自动降级。
