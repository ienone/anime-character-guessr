# 卡死监控与源码取证

`anime-character-guessr-forensics.timer` 每约 30 秒启动一次采集器。它检查
本机 `/health`、`/room-count`、角色搜索的 HTTP 状态与响应结构，记录耗时、
房间命令计数、内存、线程和文件描述符数量。角色搜索使用固定词 `saber`，
不会保存用户搜索内容；由于搜索缓存存在，这个探针不保证每次访问搜索索引。

最近 120 次采样保存在 `/run/anime-character-guessr-forensics/recent.jsonl`。
连续 3 次任一探针失败后，在 `/var/log/anime-character-guessr-forensics/`
保存采样历史、实际进程二进制校验值、构建版本、线程、连接、日志以及 GDB、
eu-stack 和短时 strace 结果。健康探针失败时跳过其他探针，减少排队。
同一次故障只采集一次；恢复健康或进程更换后重新启用。采集器不自动重启游戏服务。
GDB 等附加操作会短暂停顿进程，因此只在持续故障后触发。

## 发布要求

Rust release 保留 `line-tables-only` 调试信息，CI 开启 frame pointers 和
unwind tables，并用固定的 `/build/anime-character-guessr` 前缀编译源码路径。
这些选项保留优化；行表不包含局部变量及参数值。
参见 [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html#debug)
和 [rustc codegen options](https://doc.rust-lang.org/rustc/codegen-options/index.html#force-frame-pointers)。

`package-server.sh` 将发布包拆成以下匹配文件：

- `server-rs`：正常运行的优化程序，含外部调试文件链接。
- `server-rs.debug`：同次编译的调试信息，必须与程序一起更新。
- `source/server-rs/`：同一提交的源码和 Cargo 锁文件。
- `build-info.json`：源码提交、编译器版本和程序/调试文件 SHA-256。

部署时将这些文件安装到 `/opt/anime-character-guessr/` 的对应位置。
保留原有数据库、环境文件和缓存。GDB 自动读取旁边的 `.debug` 文件，采集器
将编译路径映射到已部署的 `source/`。升级/回滚必须成套切换上述四项。
不要用新构建的调试文件解释旧二进制；旧的 `??` 堆栈无法凭空补回丢失的帧。

## 安装与验证

依赖 Python 3、curl（人工验收）、gdb、elfutils、strace、procps、iproute2。
先备份现有脚本及 unit，再将 Python 文件以可执行权限安装为
`/usr/local/sbin/anime-character-guessr-forensics`，将同名 service/timer 安装到
`/etc/systemd/system/`，执行 `systemctl daemon-reload`，启用并启动 timer。
采集器以 root 运行，仅 root 可读新记录；不会添加公网诊断接口。

运行 `python3 -B -m unittest discover -s deploy -p 'test_forensics.py'` 验证故障
阈值、恢复后重置、PID 重用、接口异常和历史长度限制。上线前对独立端口及
数据库副本进行探针和实际取证演练，确认采集后的进程仍能响应。
上线后检查 timer、最新采样、应用健康和运行中二进制的 SHA-256。
