# 动漫角色竞猜游戏（Anime Character Guessr）重构计划

## 1. 架构目标与核心动机

面对服务器仅剩 **1GB 内存** 的严苛物理限制，原有的 Node.js + MongoDB + Bangumi API 架构由于 V8 引擎基础内存高、MongoDB 内存占用极大（约 200~300MB 起步）以及外网请求造成的阻塞，已经无法满足稳定流畅运行的需求。

本次重构旨在通过 **Rust + 纯 SQLite** 架构，实现极其严苛的资源控制和极致的请求响应速度，并达成以下三个核心优化：
1. **彻底消除 MongoDB**：改用纯 SQLite，省下数百兆的常驻内存开销。
2. **服务端 Rust 化**：内存占用预期降至 30MB ~ 50MB 级别，极速无 GC 卡顿。
3. **数据离线精简构建**：摈弃“请求时查全集”和“服务器上解析 1.6GB 垃圾数据”的做法，改为离线构建轻量级 SQLite 资产，运行期 $O(1)$ 读取。
4. **零感知前端迁移**：使用 `socketioxide` 完美兼容原版 Socket.IO 协议，前端代码基本无需调整。

---

## 2. 技术栈映射对照

| 模块 | Node.js 原版 | Rust 新版替代方案 | 说明 |
| :--- | :--- | :--- | :--- |
| **Web 框架** | Express.js | **Axum** + Tokio | 高并发、超低内存、全异步 |
| **实时通信** | Socket.IO | **Socketioxide** | **前端零修改**，100% 兼容 Socket.IO 协议 |
| **数据库** | MongoDB + Node JSON | **Rusqlite** | 极度轻量，C 层调用无序列化负担 |
| **图片转码** | Sharp (依赖 libvips) | **image** crate | 纯 Rust 实现，支持 WebP 生成，避免 C++ 依赖 |
| **异步队列** | Bull / 原生 Promise | **tokio::spawn** + mpsc | 无痛实现后台异步转码补洞、防止阻塞主线程 |

---

## 3. 数据库拓扑设计（纯 SQLite）

放弃集中式数据库，在服务器上维持 **双 SQLite 文件** 架构。

### 3.1 `archive.sqlite`（静态资产只读库）
- **特点**：像前端静态资源一样的资产文件，只读不写，由离线构建脚本打包生成。
- **作用**：存储经过高强度过滤（“硬裁剪”）后的精装数据。
- **核心表**：
  - `subjects` (过滤掉无关分类和垫底排名)
  - `characters` (过滤掉生僻配角/无头像数据)
  - `preset_pools` (预聚合的关卡随机池，如 `[2000-2010TOP百大主角 id 列表]`)
- **检索逻辑**：开局不进行 `ORDER BY RANDOM()` 全表扫，而是服务端启动时将 `preset_pools` 读入内存，随机抽取 ID 后进行主键查询（$O(1)$ 指令级耗时）。

### 3.2 `app.sqlite`（业务读写库）
- **特点**：开启 `WAL` (Write-Ahead Logging) 模式，处理日常高并发小范围读写。
- **作用**：接管原本 MongoDB 的业务与新增的缓存控制。
- **核心表**：
  - `leaderboard` / `stats`（排行榜与用户统计）
  - `image_cache`（图片转码与本地路径映射）
  - `bgm_cache`（冷门搜索引发的外网兜底请求缓存）

---

## 4. 实施路线图（分四阶段）

### Phase 1: 构建数据离线修剪工具 (CLI)
*无需部署到生产环境，在本地或 CI 执行*
1. 新建一个独立于服务端的 Rust CLI / 脚本工具。
2. 流式读取 `dump-2026-04-28.xxx` 内的 `.jsonlines`。
3. 应用过滤规则：
   - 丢弃未关联热门作品的冷门 Character。
4. 生成紧凑的 `archive.sqlite` (体积预估缩减 90%+)。

### Phase 2: Rust 基础服务端与 REST 路由封装
*接管前端基础 HTTP 交互*
1. 初始化 `server-rs` (Axum 项目)。
2. 加载双 SQLite 驱动，配置内存预设池。
3. **出题 API (`/api/game/random`)**:
   - 从内存数组掷算 ID，查询 `archive.sqlite` 返回对齐前端所需的数据结构。
4. **图片 API (`/img/character/:id`)**:
   - 查 `app.sqlite` 的 `image_cache`。未命中则立刻返回重定向（或默认占位图），同时向 Tokio 通道发送异步兜底抓取及 WebP 任务；命中则直接返回本地文件系统流。

### Phase 3: WebSocket 游戏联机核心重铸
*最复杂但性能提升最大的部分*
1. 引入 `socketioxide`。
2. 使用 `Arc<RwLock<GameState>>` 管理多人房间数据池（Room、Players、Guesses）。
3. 重构现有 `gameplay.js`，将原本松散的 JS 状态机转换成严格的 Rust 类型。
4. 提供前端所需的 `join_room`、`submit_guess`、`chat` 等所有 Socket.IO 事件的对应 Handler。

### Phase 4: 排行榜迁移与极致 Docker 部署
1. 实现 `app.sqlite` 的排行榜逻辑，替换所有旧版 MongoDB 查改代码。
2. 彻底改写 `docker-compose.yml`：
   - 删去 `mongo`、`mongo-express` 和原有 node `server` 容器。
   - 仅保留 `nginx`（可选，作无脑并发分发）和新编译的 `server-rs` 容器。
3. 编写新的 `Dockerfile`：采用跨端静态编译（`x86_64-unknown-linux-musl`），基础镜像使用最微末的 `scratch` 或 `alpine`，编译产物体积仅约 10~20MB，启动仅需几毫秒。

---

## 5. 项目结构愿景
```text
anime-character-guessr/
├── client/                 # 原有 React 客户端 (基本无改动，改下 API_BASE)
├── db-builder/             # Phase1: 离线构建工具 (Rust CLI)
├── server-rs/              # Phase2~4: Rust 服务端
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs         # 入口及 Axum 挂载
│   │   ├── config.rs       # 环境变量读取
│   │   ├── db/             # rusqlite 连接池封装 & DAO 
│   │   ├── routes/         # REST API 
│   │   ├── socket/         # Socketioxide 事件处理 (重置版 gameplay)
│   │   └── utils/          # 图片处理 (image) / HTTP请求后台抓取
│   └── Dockerfile
├── docker-compose.yml      # (缩水版: Nginx + server-rs)
└── REFACTOR_PLAN_RUST.md   # 本计划文档
```
