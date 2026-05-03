[中文](README.md) | [English](README.en.md)

## 📖 简介
二次元笑传之猜猜呗，快来弗/灯一把吧！

- 一个猜动漫角色的游戏，建议使用桌面端浏览器游玩。
- 灵感来源 [BLAST.tv](https://blast.tv/counter-strikle)，数据来源 [Bangumi](https://bgm.tv/)。
- 游玩群：467740403
- 开发交流群：894333602

## 📦 项目结构

- `client/`：Vite React 前端。
- `server-rs/`：Rust 游戏服务器，负责 Socket.IO、Bangumi API 代理和图片缓存。
- `db-builder/`：Bangumi 数据库构建与维护工具。
- `archive.sqlite`：本地条目/角色索引数据库。

## 🚀 本地运行

前端：
```bash
cd client
npm install
npm run dev
```

Rust 服务端：
```bash
cd server-rs
cargo run
```

前端通过 `VITE_SERVER_URL` 指向服务端地址；未设置时使用同源地址。

## 🐳 Docker 运行

在根目录新建 `.env` 文件：
```env
DOMAIN_NAME=http://[你的 IP]
SERVER_INTERNAL_PORT=3001
NGINX_EXTERNAL_PORT=80
AES_SECRET=YourSuperSecretKeyChangeMe
```

启动：
```bash
docker-compose up --build
```

停止并删除容器：
```bash
docker-compose down
```

## 🚀 自部署优化

- Bangumi API 通过服务端 `/api/bgm/*` 代理，避免浏览器直连 API。
- 小图使用 Bangumi `grid` 图片并缓存为本地 `/img/{id}.webp`，适合搜索结果和列表头像。
- 大图直接返回 Bangumi 源图链接，避免服务端长期存储大图。
- 可配合 CDN/边缘加速缓存 `/img/*` 和 Bangumi 源图跳转结果，降低源站压力。

## 🎮 游戏玩法

- 猜一个神秘动漫角色。搜索角色，然后做出猜测。
- 每次猜测后，你会获得你猜的角色的信息。
- 绿色高亮：正确或非常接近；黄色高亮：有点接近。
- `↑`：应该往高了猜；`↓`：应该往低了猜。

## ✨ 贡献标签

- 提交外部标签 PR 时请注意素材和数据目录结构。
- 素材文件分好文件夹，放到 `client/public/assets` 下。
- 标签数据可以直接放到 `client/public/data/extra_tags` 下，维护者会审核后导入。
- 本地测试新标签加载不出来时，检查条目 ID 是否已放进 `client/src/data/extra_tag_subjects.js`。
