# 确认 xsoc 正常运行

完成安装向导后，后台服务会持续采集和发送遥测。日常查看不需要重新运行安装或配对。

以下为 Linux 示例；Windows 使用管理员 PowerShell 去掉 sudo，macOS 使用 `/usr/local/bin/xsoc`。

```sh
sudo xsoc service status --format json
sudo xsoc pair status --format json
sudo xsoc status --check --format json
```

这三项分别回答：服务是否运行、本机绑定是什么、业务检查是否通过。最后在 xsos 中核对最新报告的接收时间。

## 查看近期活动

```sh
sudo xsoc logs --tail 100
```

查看本机待发队列：

```sh
sudo xsoc queue status --format json
```

断网时队列保存待投递报告，恢复后按原报告 ID 重试。默认容量 64 MiB，长期积压时先检查网络和服务端写入能力。需要单独检查采集，可先停服务执行 `sudo xsoc probe --format json`，完成后恢复服务；probe 不联网。

硬件空值表示尚未采集、系统不支持或权限不足，参见[硬件监控](hardware-monitoring.md)。

修改设置见[配置指南](configuration.md)，定向检查见[故障排查](troubleshooting.md)。
