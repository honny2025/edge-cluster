# Tauri 桌面壳（edge-cluster-desktop）

把 edge-cluster 平台包成一个**原生桌面窗口**：双击启动，自动拉起 Rust 服务端，窗口打开就是控制台。前端 React 代码一行没改。

> **定位：这是一个「桌面壳」。** 它不重写任何平台逻辑，只负责把平台服务端拉起来并包个窗口。
> 后端（服务端 + 手机端 Agent）在 v0.5.0-m5 已整体用 Rust 重写，这个壳拉的是 Rust 服务端二进制。

---

## 它做了什么

启动顺序（都在 `src/lib.rs` 里，可读）：

1. **找项目根目录**：`EDGE_CLUSTER_ROOT` 环境变量 → 可执行文件所在目录逐级向上 → 当前目录逐级向上，找含 `server-rust/config.toml` 的目录。
2. **拉起服务端**：跑 `server-rust` 编译出的 `edge-cluster-server` 二进制，工作目录设为 `server-rust/`（这样配置、SQLite、模型、日志、`web/dist` 的相对路径才能解析对）。如果平台已经在跑，就**不重复启动**。
3. **等就绪**：TCP 探健康接口 `GET /api/v1/system/health`，最多等 30 秒。
4. **开窗**：打开一个 1280×800 的 WebView 窗口，直接加载 `http://127.0.0.1:8000`（平台单端口交付，前端和后端同一个端口）。
5. **退出收尾**：窗口关掉时，把第 2 步自己拉起的服务端进程顺手结束，不留孤儿进程。

窗口里显示的就是原样的 React 控制台，`/api` 和 `/ws` 都是相对路径，天然指向 8000，所以前端不用做任何改动。

服务端二进制查找顺序（`server_binary()`）：
`server-rust/target/release/` → `server-rust/target/debug/` → `server-rust/`（平铺）。
这样开发态（cargo run）和发布态（二进制拷到 `server-rust/` 下）都能找到。

---

## 前置条件

| 依赖 | 说明 |
|---|---|
| **Rust 工具链** | [rustup](https://rustup.rs) 安装。注意：Tauri 在 Windows 上需要 **MSVC** 工具链（`stable-x86_64-pc-windows-msvc`），本机当前只有 GNU 工具链，见下方「编译状态」 |
| **MSVC 生成工具** | 微软 [Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)，勾选「使用 C++ 的桌面开发」工作负载（Tauri 在 Windows 上靠 MSVC 链接 + 编译 Windows 资源） |
| **WebView2 运行时** | Win10/11 一般自带，不用管 |

---

## 编译状态（诚实说明）

- 本机当前 Rust 工具链是 **GNU**（`stable-x86_64-pc-windows-gnu`），**没有安装 MSVC**。
  服务端（`server-rust`）和手机端 Agent（`node-agent-rust`）都是用 GNU 工具链编译通过的。
- **但这个 Tauri 壳在 Windows 上需要 MSVC 才能链接**（`webview2-com` 等 Windows 依赖 + 资源编译），
  所以 `src/lib.rs` 的改动**尚未在本机编译验证**。装好 MSVC 后按下面的命令跑一遍即可，
  如果报某个 API 名字对不上，把报错发我，我立刻改。

---

## 怎么跑

装好 Rust（MSVC 工具链）后，在 `web/` 目录里：

```powershell
# 开发态：编译并直接跑（窗口打开 = 服务端被拉起 + 控制台出现）
cargo tauri dev

# 发布态：先构建前端 + 服务端，再打出独立 exe
cd web && npm run build            # 生成 web/dist（frontendDist）
cd ../server-rust && cargo build --release   # 生成 edge-cluster-server
cd ../web/src-tauri && cargo build --release
# 产物在 web/src-tauri/target/release/edge-cluster-desktop.exe
```

> 想用 Tauri 官方 CLI 打安装包（`npx tauri build`，产出 .msi/.exe 安装器）也可以，
> 那会额外要求 WiX / NSIS 等打包工具，第一次建议先用 `cargo build --release` 拿到 exe 就跑。

---

## 环境变量

| 变量 | 作用 |
|---|---|
| `EDGE_CLUSTER_ROOT` | 指定项目根目录（含 `server-rust/config.toml` 的那个）。exe 和项目不在同一目录时用它兜底 |

---

## 当前限制（诚实说明）

1. **它不打包服务端二进制。** 这个壳假设项目目录（含 `server-rust/` 及其二进制）就放在能找到的地方。
   做成一个「拷给别人、双击即用」的**单文件安装包**，需要下一步把 `edge-cluster-server` 打成 Tauri sidecar，那是另一件事，本壳没做。
2. **端口写死 8000。** 换端口要同步改 `src/lib.rs` 的 `SERVER_URL`、`server-rust/config.toml`、`start_all.ps1` 三处。
3. **图标是占位图**（一个蓝方块），正式发布前换成真 logo，替换 `icons/icon.png` 和 `icons/icon.ico` 即可。

---

## 文件清单

```
src-tauri/
├── Cargo.toml            Rust 依赖与配置
├── build.rs              Tauri 构建脚本
├── tauri.conf.json       Tauri 应用配置（frontendDist 指向 ../dist，窗口在代码里手动建）
├── frontend/index.html   占位页（保留，实际窗口加载 8000 端口，不显示它）
├── icons/                icon.png / icon.ico（占位图标）
└── src/
    ├── main.rs           入口
    └── lib.rs            核心：找根目录 → 拉起服务端 → 探活 → 开窗 → 收尾
```
