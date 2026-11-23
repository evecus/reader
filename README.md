# legado-server

服务端版 Legado：Rust 实现的 Legado 书源解析引擎 + 内嵌 Web 前端，单二进制、本地单用户。

以 [gedoor/legado](https://github.com/gedoor/legado) 的规则引擎为核心参考，直接消费 Legado 书源 JSON（无需改写为插件）。

## 功能

- **书源**：Legado JSON 导入（单个/数组/订阅 URL）、启用停用、测试、删除
- **搜索**：多书源并发搜索、结果按书名+作者合并来源
- **发现**：书源 exploreUrl 分类/榜单
- **阅读**：书籍详情、目录（翻页/倒序）、正文（jsLib 清洗）
- **书架**：加入/移除、阅读进度
- **缓存**：章节离线缓存与管理
- **整书下载**：一键后台下载整本书（并发抓章、实时进度、可取消），缓存持久化到 SQLite
- **主题**：棕色顶栏 + 红色强调（参考 Legado 原生 App），提供 5 套阅读背景

## 规则引擎支持

| 语法 | 支持 |
|---|---|
| CSS 选择器 + `@text/@html/@href/@src` 修饰符 | ✅ |
| XPath 1.0（`@XPath:` / `/` / `//` 开头） | ✅ 完整（sxd-xpath，支持轴/谓词/函数/位置） |
| `@Json:` / `$.` / `$[` | ✅ |
| `@js:` / `<js>` / `{{...}}`（jsLib） | ✅ QuickJS |
| `##` 正则替换 / `&&` `\|\|` `%%` 组合 | ✅ |
| `@get:{}` / `@put:{}` 变量 | ✅ |

## 运行

```bash
# 编译（前端需先 npm run build，dist 内嵌进二进制）
cd web && npm install && npm run build && cd ..
cargo build --release

# 运行
DATA_DIR=./data PORT=5888 ./target/release/legado-server
```

打开 `http://localhost:5888`。

Docker：

```bash
docker build -t legado-server .
docker run -d -p 5888:5888 -v ./data:/data legado-server
```

## API

| 端点 | 说明 |
|---|---|
| `GET /api/health` | 健康检查 |
| `GET/POST /api/sources` | 书源列表 / 导入 |
| `PATCH/DELETE /api/sources/{url}` | 启用切换 / 删除 |
| `POST /api/sources/test` | 测试书源 |
| `GET /api/search?key=&page=&origin=` | 搜索 |
| `GET /api/explore?origin=&url=&page=` | 发现 |
| `GET /api/book/info?origin=&url=` | 详情 |
| `GET /api/book/chapters?origin=&url=&toc_url=` | 目录 |
| `GET /api/book/content?origin=&url=&book_url=` | 正文 |
| `GET/POST/DELETE /api/shelf` | 书架 |
| `PATCH /api/shelf/progress` | 阅读进度 |
| `GET/POST/DELETE /api/cache` | 缓存 |
| `GET/POST/DELETE /api/download` | 后台下载任务：列表 / 启动 / 清除历史 |
| `GET/DELETE /api/download/{id}` | 查询进度 / 取消任务 |

## 目录结构

```
src/
├── engine/          # 规则引擎：切分器/求值器/JS/选择器/XPath
├── book.rs          # 四条业务链路
├── downloader.rs    # 整书后台下载任务管理
├── fetcher.rs       # URL 替换/编码探测/HTTP
├── db.rs            # SQLite 存储
├── api/             # REST 路由
└── main.rs          # 服务入口 + 静态资源内嵌
web/src/pages/       # 8 个前端页面
```

## 说明

本项目为通用阅读工具，不内置任何书源内容。请自行导入合法书源并遵守当地法律。
