# reader

服务端版阅读器：Rust 实现的 Legado 书源解析引擎 + 内嵌 Web 前端，单二进制、本地单用户。

以 [gedoor/legado](https://github.com/gedoor/legado) 的规则引擎为核心参考，直接消费 Legado 书源 JSON（无需改写为插件）。

## 功能

- **书源**：本地 JSON 文件 / 远程订阅链接导入（单个/数组）、启用停用、测试、删除
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
| Legado 默认语法：`class.x@tag.a.0@text`、`id.`、`tag.`、`text.`、`children`、`!` 排除、`[1,-1,2:5]` 索引/区间/反向，其余回退 CSS 选择器 | ✅ 移植自官方 `AnalyzeByJSoup` |
| `@CSS:` 前缀：纯 CSS 选择器 + 末尾 `@text/@html/@href/@src` 等修饰符 | ✅ |
| 目录 `nextTocUrl` / 正文 `nextContentUrl` 翻页 | ✅ 单个下一页顺序翻、多个并发抓；URL 去重防循环；正文遇到下一章 URL 即停 |
| XPath 1.0（`@XPath:` / `/` / `//` 开头） | ✅ 完整（sxd-xpath，支持轴/谓词/函数/位置） |
| `@Json:` / `$.` / `$[` / `$` 根 | ✅ |
| `@js:` / `<js>` / `{{...}}`（jsLib） | ✅ QuickJS |
| jsUrl：searchUrl/exploreUrl 用 `<js>...</js>` 脚本构造 | ✅（注入 key/page/source/java.ajax） |
| `<js>...</js>` 前缀 + 选择器组合（如目录 `<js>...</js>$[*]`） | ✅ |
| `url,{"method":"POST","body":...}` 请求选项（method/body/headers/charset/retry/timeout） | ✅ 四链路统一消费，支持 POST 搜索 |
| `java.ajax({url,method,body,headers})` 带选项请求 | ✅ headers/method/body 不再丢弃 |
| `##` 正则替换 | ✅ |
| `&&` 并集 / `\|\|` 择一 / `%%` 交织 组合符 | ✅（按官方 `AnalyzeByJSoup` 语义） |
| `a@text` 等多步 CSS 链修饰规则 | ✅ |
| 目录 `-` 前缀（站点目录是倒序，需反转；无前缀保持页面顺序） | ✅ 并按章节 URL 去重 |
| `@get:{}` / `@put:{}` 变量 | ✅ |
| `java.put/get` + `source.getVariable/setVariable` 变量持久化（SQLite） | ✅ 跨请求保留登录态 |
| Cookie jar（域级隔离、Set-Cookie 自动回写、Cookie 请求头注入、`java.getCookie`） | ✅ 进程级共享 |
| 内置 CryptoJS（MD5/SHA1/SHA256/AES/Hmac/Hex/Base64） | ✅ 官方 64KB 原版按需注入，签名型书源可用 |
| `java.md5Encode/md5Encode16/digestHex/digestBase64Str/HMacHex/HMacBase64` | ✅ 对齐官方 `JsEncodeUtils` |
| `java.aesEncode/aesDecode`（ECB/CBC，PKCS5/7/NoPadding，128/192/256） | ✅ + ArgsBase64 变体 |
| 响应编码：显式 charset / meta charset 探测（GBK/GB18030/Big5 等） | ✅ |

> 说明：依赖 Legado 私有运行机制（如 `java.startBrowser()`、VIP 渲染面板、WebView 动态渲染、字体反混淆等）的聚合/插件型书源，可能仅搜索/详情/目录可用，正文无法完整复现。变量与 Cookie 已跨请求持久化，登录型书源多数场景可读正文；内置 CryptoJS 与 java.* 加密绑定覆盖了签名型书源（无需书源自带 jsLib）。仍缺的集中在 `@webjs:`/WebView 渲染、字体反混淆、DES/非对称加密等。

## 运行

```bash
# 编译（前端需先 npm run build，dist 内嵌进二进制）
cd web && npm install && npm run build && cd ..
cargo build --release

# 运行
DATA_DIR=./data PORT=5888 ./target/release/reader
```

打开 `http://localhost:5888`。

Docker：

```bash
docker build -t reader .
docker run -d -p 5888:5888 -v ./data:/data reader
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
├── engine/          # 规则引擎：切分器/求值器/JS/选择器/XPath/加密
│   └── crypto.rs    # 摘要/HMac/AES（对齐 java.* 加密方法）
├── assets/
│   └── cryptojs.min.js  # 官方内置 CryptoJS（按需注入 QuickJS）
├── book.rs          # 四条业务链路
├── downloader.rs    # 整书后台下载任务管理
├── fetcher.rs       # RequestSpec 选项解析/编码探测/HTTP（含 POST/Cookie/charset/retry）
├── cookie.rs        # 轻量 Cookie jar（域级隔离 / Set-Cookie 回写）
├── db.rs            # SQLite 存储（书源变量持久化表）
├── api/             # REST 路由
└── main.rs          # 服务入口 + 静态资源内嵌
web/src/pages/       # 8 个前端页面
```

## 说明

本项目为通用阅读工具，不内置任何书源内容。请自行导入合法书源并遵守当地法律。
