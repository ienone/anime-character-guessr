# 性能基准测试

对比 Node.js 和 Rust（server-rs）后端在各核心接口的真实响应延迟与吞吐量。

## 快速使用

```bash
cd benchmarks

# 测试 Rust 后端（服务需在 3001 端口运行）
node run_bench.js --target rust

# 测试 Node.js 后端（服务需在 3000 端口运行）
node run_bench.js --target node

# 同时对比两个后端（高负载）
node run_bench.js --target both --concurrency 20 --requests 500

# （可选）由脚本自动启动服务并测量启动时间（成功后自动关闭进程）
node run_bench.js --spawn --target both --concurrency 20 --requests 500
```

## 后端卡死/SQLite 压力排查

`stress_server.js` 用于把本地读、app.sqlite 写入、图片缓存查询、外网图片源兜底分开压测：

```bash
cd benchmarks

# 混合本地读写，不主动触发外网图片源兜底
npm run stress -- --scenario mixed --concurrency 80 --requests 2000

# 只压 archive.sqlite / Tantivy 读路径
npm run stress -- --scenario db-read --concurrency 120 --requests 3000

# 只压 app.sqlite 写入路径
npm run stress -- --scenario app-write --concurrency 80 --requests 2000

# 单独测试图片源兜底；该场景会受 BGM 外网影响
npm run stress -- --scenario image-source --concurrency 20 --requests 200 --timeoutMs 12000

# Socket.IO 多人房间压测：10 个房间，每房 4 人，每个非房主连续提交 40 次猜测
npm run stress:socket -- --rooms 10 --players 4 --guesses 40
```

结果会写入 `benchmarks/results/stress_*.json`，摘要包含状态码、超时数、慢请求数、P95/P99 和按路径聚合的最大延迟。

## 命令行参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--mode` | `server-compare` | `server-compare`（对比 Node vs Rust）/ `api-compare`（直连 BGM vs 本地一次调用） |
| `--target` | `both` | `node` / `rust` / `both` |
| `--concurrency` | `10` | 并发请求数 |
| `--requests` | `200` | 每个场景总请求数 |
| `--nodeUrl` | `http://localhost:3000` | Node.js 服务地址 |
| `--rustUrl` | `http://localhost:3001` | Rust 服务地址 |
| `--output` | `./results` | 结果 JSON 保存目录 |
| `--sampleMs` | `250` | 压测期间 `/metrics` 采样间隔（ms） |
| `--warmupMs` | `500` | `/metrics` 采样前热身等待（ms） |
| `--spawn` | `false` | 是否由脚本自动启动服务并测量启动时间（成功后自动关闭进程） |
| `--startupTimeoutMs` | `20000` | 启动后等待 `/health` 通过的超时时间（ms） |
| `--nodeCmd` | `node server.js` | `--spawn` 时启动 Node 服务的命令（工作目录为 `server/`） |
| `--rustCmd` | `cargo run --release` | `--spawn` 时启动 Rust 服务的命令（工作目录为 `server-rs/`） |

---

## 实测结果（Rust server-rs）

> 测试环境：Windows 11，archive.sqlite 约 127 MB（59,357 个角色，17,531 个作品，180,866 条关联关系）  
> 运行参数：`--mode server-compare --target rust --concurrency 20 --requests 500 --sampleMs 250 --warmupMs 500`（并发 20，共 500 请求/场景）

| 场景 | 说明 | 平均延迟 | P50 | P95 | P99 | RPS | 错误数 |
|------|------|----------|-----|-----|-----|-----|--------|
| `health_check` | 健康检查（无 IO） | 3.6 ms | 2.8 ms | 5.4 ms | 16.7 ms | 5,376 | 0 |
| `game_random_character` | 随机角色（全内存，零 DB 查询）| **3.5 ms** | 3.1 ms | 5.9 ms | 12.8 ms | **5,556** | **0** |
| `room_count` | 当前房间数 | 2.7 ms | 2.2 ms | 5.2 ms | 12.7 ms | 7,353 | 0 |
| `leaderboard` | 积分榜查询 | 3.3 ms | 2.9 ms | 6.0 ms | 14.1 ms | 5,952 | 0 |
| `roulette` | 随机10角色轮盘 | 27.0 ms | 26.5 ms | 30.7 ms | 41.9 ms | 740 | 0 |

### 说明

- **全内存零 DB 查询**：启动时加载 59,357 条角色 JSON + 180,866 条 subject 数据到内存，运行时不再访问 SQLite。
- **冷启动索引构建**：首次启动约需 **4.4 秒**建立内存缓存；此后每次请求均为纯内存操作。
- **`game_random_character` 与 `health_check` 同级**：3.5 ms avg vs 3.6 ms — 说明角色组装计算量已可忽略不计。
- **零错误**：并发 20 下 500 次请求全部成功，无 TCP 错误、无 500 错误。

### 与原版单人模式（直连 Bangumi API）对比

| 指标 | 直连 BGM API | Rust 后端（本版本）|
|------|-------------|------------------|
| 平均延迟（热） | 1,500–8,000 ms（国内网络） | **3.5 ms** |
| P95 延迟 | 15,000 ms+ | **5.9 ms** |
| 并发 20 错误率 | N/A（单次串行调用） | **0%** |
| 依赖外部网络 | ✅ 受 BGM 服务可达性影响 | ❌ 完全离线，无网络依赖 |
| 开局 API 调用次数 | 10–30 次串行请求 | **0 次**（全内存）|
| 角色标签计算位置 | 前端 JS（阻塞渲染） | 后端 Rust（并发处理）|
| 吞吐量 | ~0.1 RPS（受 BGM 速率限制） | **5,556 RPS** |

---

## 监控端点

两个服务均暴露实时性能指标：

```bash
# Rust 服务
curl http://localhost:3001/metrics          # Prometheus 文本格式
curl http://localhost:3001/metrics          # （同上，含直方图分桶）

# Node.js 服务
curl http://localhost:3000/metrics          # Prometheus 文本格式
curl http://localhost:3000/metrics/json     # JSON 格式（含 per-route 统计）
```

**Prometheus 指标示例：**

```
http_requests_total 2506
http_requests_slow 427           # >500ms 慢请求计数
http_latency_avg_ms 163
http_request_duration_ms_bucket{le="10"}   2506
http_request_duration_ms_bucket{le="50"}   1004
http_request_duration_ms_bucket{le="100"}  718
http_request_duration_ms_bucket{le="500"}  498
http_request_duration_ms_bucket{le="1000"} 427
process_rss_kb 42816             # 进程内存（Linux）
```

---

## 前端性能数据

在浏览器控制台访问 API 延迟统计：

```js
import { perf } from './src/utils/perf.js'

perf.printReport()   // 打印 P50/P95 延迟表格
perf.getReport()     // 返回 JSON 数据
perf.reset()         // 清空统计（用于分场景测试）
```

输出示例：

```
[PerfMonitor] Request Summary
Route                        Count  Avg(ms)  P50(ms)  P95(ms)  Errors
POST /api/game/random          12     14.3     12.1     18.7      0
POST /api/game/character        8     11.2     10.8     15.3      0
```

---

## 结果文件

每次运行的原始数据保存在 `results/` 目录：

```
results/
  rust_1746172000000.json
  node_1746172000000.json
  server_compare_1746172000000.json
  api_compare_1746172000000.json
```

JSON 结构：`[{ scenario, requests, errors, durationMs, rps, avgMs, p50Ms, p95Ms, p99Ms }]`

---

## 性能优化说明

### 候选池缓存（CandidatePool）

`GET /api/game/random` 会按筛选设置缓存候选角色 ID 池，而不是缓存完整角色详情。缓存命中后只从候选 ID 中抽样，再按 ID 组装本次答案详情。

- TTL：6 小时
- 上限：128 个筛选组合
- 内容：`Vec<i64>` 候选角色 ID
- 构建锁：同一筛选组合只有一个线程构建，避免多人同时开局时重复扫 SQLite

这种方式降低多人同时开局时的候选筛选压力，同时避免把完整角色详情长期堆在内存里。

### SQLite 并发限制

archive.sqlite 为只读连接池（32 连接），SQLite WAL 模式下多读并发表现良好；  
但 `spawn_blocking` 线程数受 tokio 阻塞线程池限制，真实并发 > 10 时建议配合反向代理（nginx）做水平扩展，或考虑将热点数据迁移至 Redis 缓存。
