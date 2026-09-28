# 题库更新

`refresh-data.py` 下载 Bangumi 官方最新 dump 并验证 SHA-256，使用现有 `db-builder` 构建题库、Tantivy 索引和声优缓存。角色特征标签是另外维护的静态资源，不由 dump 更新。

只构建数据包：

```sh
python3 deploy/refresh-data.py --builder /path/to/db-builder --output /path/to/new-snapshot
```

输出包括 `archive.sqlite`、`tantivy/`、`cache.sqlite` 和 `data-version.json`。`cache.sqlite` 只包含导入缓存，不能直接替换线上 `app.sqlite`，否则会丢失排行榜。

systemd 部署中，将发布包的 `db-builder`、`refresh-data.py` 放入 `/opt/anime-character-guessr/`，安装 `anime-character-guessr-data.service` 和 `.timer` 后运行：

```sh
systemctl daemon-reload
systemctl enable --now anime-character-guessr-data.timer
```

每天北京时间 04:15 检查官方快照；摘要未变则跳过。有房间时不更新，等下一次检查。更新前在旁边构建完整数据包，最后再次确认空房间，停止服务、备份数据库和旧索引，再成套切换。应用数据库仅刷新声优表，保留图片缓存与排行榜。新数据启动失败时恢复备份。构建失败不动线上数据。

备份位于 `releases-backup/*-data`；自动保留最近两份数据更新备份。该服务限制为半个 CPU 核和 600MB 内存。首次安装前需确认磁盘有足够空间下载和解压题库；程序只解压构建所需的五个文件，结束即删除中间文件。

主程序启动会读取索引快照，因此数据切换需要重启。首页通过 `/api/archive/version` 显示题库来源日期。

代理部署使用 `HOST=127.0.0.1` 和 `TRUST_PROXY_HEADERS=true`，Nginx 覆写 `X-Real-IP`。Docker 部署继续使用容器内部地址和已有代理设置。
