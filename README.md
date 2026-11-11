# reader — 自托管阅读器（Rust 版）

单二进制 Rust 后端 + 内嵌 web 前端的自托管阅读器。兼容原 Node 版的 `reader.db`：账号、书架、阅读进度、笔记、元数据覆盖与订阅数据原样复用，可以直接把原版数据目录挂进来继续用。

## 特性

- **零外部运行时**：前端静态资源在编译期内嵌（rust-embed），插件用内嵌 QuickJS 引擎执行（rquickjs），运行时不需要 Node、Python 或任何解释器。
- **仅 Linux**：musl 静态编译，amd64 / arm64 双架构镜像。
- **数据兼容**：schema 与增量迁移和 Node 版逐字节一致；JWT（HS256）与 refresh token 哈希格式一致，旧会话不失效。
- **阅读功能完整**：本地书库扫描（EPUB / TXT / CBZ / PDF / 图片 / 图片目录漫画 comic-dir）、阅读进度同步、笔记、OPDS 客户端 + 服务端、书源与 QuickJS 插件、TTS / AI 代理。
- **影音前端已删干净**：没有影音路由、页面、组件或播放器；旧 `/media/*` 深链统一回落到书架。默认路由与兜底路由均为 `/shelf`。
- **media.db 完全不碰**：影音后端不在本版本范围内，`/api/v1/media/*` 只返回兼容空响应，media.db 原样保留。

## 快速开始

```bash
# 直接运行（目录默认 /books 与 ./data，可用环境变量覆盖）
BOOKS_DIR=/path/to/books DATA_DIR=/path/to/data ./reader

# Docker
docker run -d -p 8080:8080 \
  -v /path/to/books:/books \
  -v /path/to/data:/data \
  ghcr.io/163jm/reader-rs:latest
```

裸二进制默认监听 `5888`（沿用原 Node 版端口），打开 `http://<host>:5888`；Docker 镜像内设了 `PORT=8080`，对应上面的 `-p 8080:8080`。注册的第一个账号自动成为管理员。

## 环境变量

| 变量 | 默认值 | 说明 |
| --- | --- | --- |
| `BOOKS_DIR` | `/books` | 书籍目录（可只读挂载） |
| `DATA_DIR` | `./data` | 数据目录（reader.db、封面、插件，需可写） |
| `HOST` | `0.0.0.0` | 监听地址 |
| `PORT` | `5888`（镜像内 `8080`） | 监听端口，沿用 Node 版默认值 |
| `JWT_SECRET` / `READER_TOKEN_SECRET` | 自动生成 | 会话签名密钥；与原版共用数据目录时**必须设置成原版的值**，否则旧会话失效 |
| `ACCESS_TOKEN_TTL` / `REFRESH_TOKEN_TTL` | 秒数 | 会话有效期 |
| `SCAN_INTERVAL` | 秒 | 后台扫描间隔，0 关闭 |
| `CORS_ORIGINS` | 空=反射任意来源 | 逗号分隔的来源白名单 |

## 插件（QuickJS）

插件是独立子进程（`reader plugin-run <dir>`），宿主与插件之间走 stdio NDJSON JSON-RPC——与 Node 版线协议一致，只是插件进程内跑的是内嵌 QuickJS 而不是 Node。因此插件**不能使用 npm 依赖**，是一个自包含脚本：

```js
// main.js
globalThis.methods = {
  browse(params) { return { items: [...], nextCursor: null }; },
  search(params) { return { items: [...], nextCursor: null }; },
  detail(params) { return { ref: params.entryRef, title: "..." }; },
};
```

宿主注入的能力：`host.fetchJson(url, options)`、`host.hash('sha256', text)`、`host.log(msg)`、`host.dataDir()`、`host.instanceDataDir()`、`host.storage.read/write(key, value)`（作用域限定在插件自己的数据目录）。

安装：管理页面 → 书源 → 上传 `.tgz`（`npm pack` 产物布局，`plugin.json` 的 `"runtime": "quickjs"`）。

## 从 Node 版迁移

1. 停掉 Node 版。
2. 把同一个 `BOOKS_DIR` / `DATA_DIR` 挂给 Rust 版（`media.db` 会留在原地不被触碰）。
3. 如果原版设了 `READER_TOKEN_SECRET` 环境变量，给 Rust 版设同样的值。
4. 启动即可：登录态、书架、进度全部延续。

## 构建

```bash
npm ci && npm run build   # 产出 web/dist（构建期需要 Node，运行时不需要）
cargo build --release     # web/dist 会被内嵌进二进制
```

CI（`.github/workflows/release.yml`）会产出 amd64/arm64 musl 静态二进制与 Docker 镜像。
