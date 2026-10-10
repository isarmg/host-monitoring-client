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

## 平台日志与路径

- Linux：`sudo journalctl -u xsoc.service -n 100 --no-pager`
- macOS：`sudo tail -n 100 /var/log/xsoc.log`；安装日志在 `/var/log/install.log`
- Windows：`xsoc logs --tail 100 --format json`，早期启动失败查看 SCM 服务退出码和系统事件

Windows 持久 JSON 日志默认每份 8 MiB，活动文件加四份归档共 40 MiB。可用 `logs --follow --format ndjson --timeout 60s` 有界跟踪。日志读到损坏或已过保留窗口会明确报错。

默认配置：Windows `C:\ProgramData\xsoc\config.json`，Linux `/etc/xsoc/config.json`，macOS `/Library/Application Support/xsoc/config.json`。实际状态目录以 `config show` 中 state_dir 为准，Linux 默认 `/var/lib/xsoc`。

## 修复或卸载当前安装

同版原生包可修复程序与服务文件，保留业务状态。Windows MSI 完成后检查退出码，3010 表示需重启；Linux DEB 可用 `sudo apt install --reinstall ./xsoc_1.0.0_amd64.deb`；macOS 重新执行已校验 PKG 的 installer 命令。随后检查版本、服务和业务连接。

普通卸载步骤见[安装指南](platform-setup.md)。若已确认永久退役、服务端实例已撤销且待发数据可丢弃，Windows MSI 的 `PURGE=1`、Debian 的 `sudo apt purge xsoc` 或 RPM 卸载前的 `sudo xsoc-purge --yes`、macOS 助手 `uninstall.sh --purge` 会清除安装器管理的本地状态。普通排障保留状态即可，无需清除。
