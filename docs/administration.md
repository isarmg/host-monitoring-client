# 维护 xsoc

以下命令在客户端主机执行。Linux 使用 sudo；macOS 用 `sudo /usr/local/bin/xsoc`；Windows 管理员 PowerShell 中直接使用 `xsoc`。

## 服务启停与自启

| 目的 | 命令 |
|---|---|
| 查看登记、运行和自启 | `xsoc service status --format json` |
| 立即启动、停止、重启 | `xsoc service start`、`stop`、`restart`，选择一项 |
| 设置开机自启 | `xsoc service enable` |
| 取消开机自启 | `xsoc service disable` |
| 取消自启并立即停止 | `xsoc service disable --now` |

停止 xsoc 会暂停采集，已有队列保留供再次启动时处理。

## 更新实例授权码

管理员更换同一实例的授权码后，停止客户端，输入新码并检查保存结果：

```sh
sudo xsoc service stop
sudo xsoc pair recover --interactive
sudo xsoc pair status --format json
sudo xsoc service start
sudo xsoc status --check --format json
```

原主机 UUID 与待发队列保留。若只是配对请求响应丢失，使用 `pair resume` 继续现有事务。

## 处理队列

`queue status` 查看数量与容量，`queue inspect` 查看有界摘要。需要主动排空时，先停服务，运行 `sudo xsoc queue drain --timeout 10m --format json`，成功后再启动服务；这会向原服务端实际投递。

更换实例而放弃旧绑定时，先排空原队列，然后使用 `pair replace --confirm-replace --expected-binding CURRENT_HOST_UUID --interactive`，UUID 从 `pair status` 取得。旧服务端永久丢失时，可用 `queue archive --reason server-state-lost` 保留无法投递的原队列；归档不表示送达，也不把旧报告归给新身份。

## 本机日志、修复和卸载

按所用平台查看日志路径、同版包修复及卸载步骤：

- [Linux](platforms/linux.md#日志与本机状态)
- [Windows](platforms/windows.md#日志与本机状态)
- [macOS](platforms/macos.md#日志与本机状态)

永久清除状态会丢失本机身份和未投递报告；只有确认退役且数据可丢弃后才使用平台指南中的清理选项。
