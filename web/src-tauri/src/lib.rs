//! edge-cluster 桌面壳的后端逻辑（Rust 版）。
//!
//! 这个 crate 不重写任何平台逻辑，只做两件事：
//!   1) 找到项目里的平台服务端二进制（server-rust 编译出的 edge-cluster-server），拉起来；
//!   2) 打开一个 WebView 窗口，指向 http://127.0.0.1:8000（平台单端口交付）。
//!
//! 前端 React 代码、后端服务端逻辑一行都不动，窗口里跑的就是原样的控制台。
//!
//! 迁移说明：v0.5.0-m5 之前这里拉的是 Python 的 server/run.py；现在后端整体换成 Rust，
//! 所以改成拉起 server-rust 编译出的 edge-cluster-server 二进制。wire 协议、端口、SQLite
//! 表结构都不变，所以窗口里的前端代码无需任何改动。

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::webview::DownloadEvent;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// 平台服务端地址。端口写死 8000，与 server-rust/config.toml 的默认值一致。
/// 若要换端口，需同时改这里、server-rust/config.toml，以及 start_all.ps1 的 -Port。
const SERVER_URL: &str = "http://127.0.0.1:8000";

/// 服务端二进制名（server-rust 的 package name）。Windows 下带 .exe 后缀。
fn server_bin_name() -> &'static str {
    if cfg!(windows) {
        "edge-cluster-server.exe"
    } else {
        "edge-cluster-server"
    }
}

/// 我们拉起来的平台子进程。App 退出时顺手收掉，避免留下孤儿进程。
struct ServerProcess(Mutex<Option<Child>>);

/// 找到 edge-cluster 项目根目录（含 server-rust/config.toml 的那个目录）。
///
/// 优先级：环境变量 EDGE_CLUSTER_ROOT > 可执行文件所在目录逐级向上 > 当前工作目录逐级向上。
/// 这个「逐级向上找」是为了兼容开发态（cargo run 时 cwd 是 src-tauri）和
/// 发布态（exe 可能被放进项目目录、也可能放在项目外的安装目录里，靠环境变量兜底）。
fn project_root() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("EDGE_CLUSTER_ROOT") {
        let pb = PathBuf::from(p);
        if pb.join("server-rust").join("config.toml").exists() {
            return Some(pb);
        }
    }

    let mut starts = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            starts.push(dir.to_path_buf());
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        starts.push(cwd);
    }

    for start in starts {
        let mut p = start;
        loop {
            if p.join("server-rust").join("config.toml").exists() {
                return Some(p);
            }
            if !p.pop() {
                break;
            }
        }
    }
    None
}

/// 在项目根目录下找服务端二进制。
///
/// 依次尝试 release / debug / 平铺在 server-rust 下的三种布局，兼容开发态与发布态：
///   - server-rust/target/release/edge-cluster-server[.exe]   （发布构建）
///   - server-rust/target/debug/edge-cluster-server[.exe]     （开发构建）
///   - server-rust/edge-cluster-server[.exe]                  （打包时平铺进去）
fn server_binary(root: &Path) -> Option<PathBuf> {
    let name = server_bin_name();
    let base = root.join("server-rust");
    for rel in [
        PathBuf::from("target").join("release").join(name),
        PathBuf::from("target").join("debug").join(name),
        PathBuf::from(name),
    ] {
        let cand = base.join(rel);
        if cand.exists() {
            return Some(cand);
        }
    }
    None
}

/// 用裸 TCP 请求探一下健康检查接口，判断平台是否已经就绪。
/// 不引入 HTTP 客户端依赖 —— 几个字节就能拿到答案。
fn health_ok(url: &str) -> bool {
    let hostport = url
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');

    if let Ok(mut s) = TcpStream::connect(hostport) {
        let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
        let req = format!("GET /api/v1/system/health HTTP/1.0\r\nHost: {hostport}\r\n\r\n");
        if s.write_all(req.as_bytes()).is_ok() {
            let mut buf = [0u8; 512];
            if let Ok(n) = s.read(&mut buf) {
                let resp = String::from_utf8_lossy(&buf[..n]);
                // 200 说明在服务；"status" 说明返回的确实是健康检查的 JSON。
                return resp.contains("200") && resp.contains("status");
            }
        }
    }
    false
}

/// 启动平台子进程（server-rust 的 edge-cluster-server）。
/// 工作目录设为 server-rust，这样配置、SQLite、模型、日志、web/dist 的相对路径才能解析对。
/// 失败返回 None，由调用方决定是否报错。
fn spawn_server(root: &Path) -> Option<Child> {
    let bin = server_binary(root)?;
    Command::new(&bin)
        .current_dir(root.join("server-rust"))
        .spawn()
        .ok()
}

/// Tauri 主流程。窗口在 setup 里手动创建，所以 tauri.conf.json 里 windows 是空数组。
pub fn run() {
    // 这个 exe 只是「桌面壳」，必须能顺着找到项目里的后端（server-rust/config.toml）才能工作。
    // 如果被人单独复制到桌面等别处，project_root() 会找不到，下面原来的 .expect 会直接 panic——
    // 而 GUI 程序 panic 时没有控制台，表现就是「双击没反应」。这里改成弹框说明原因，而不是静默闪退。
    if project_root().is_none() {
        rfd::MessageDialog::new()
            .set_title("Edge Cluster 无法启动")
            .set_description(
                "找不到项目目录（里面应有 server-rust/config.toml）。\n\n\
                 这个 exe 只是「桌面壳」，它要拉起项目里的后端 edge-cluster-server 才能工作，\
                 所以不能单独复制到桌面。\n\n\
                 请这样用：\n  - 把它放回整个项目文件夹（或解压后的便携包目录）里再双击；\n  \
                 - 或保留桌面的「EdgeCluster」快捷方式，让快捷方式去指项目里的 exe；\n  \
                 - 或设置环境变量 EDGE_CLUSTER_ROOT 指向项目目录。",
            )
            .set_level(rfd::MessageLevel::Error)
            .show();
        return;
    }

    tauri::Builder::default()
        .manage(ServerProcess(Mutex::new(None)))
        .setup(|app| {
            let root = project_root().expect(
                "找不到 edge-cluster 项目根目录（含 server-rust/config.toml）。请设环境变量 EDGE_CLUSTER_ROOT 指向它。",
            );

            // 平台可能已经在跑（用户之前双击过 启动.bat）。通则不重复启动，避免端口冲突。
            let mut started_here = false;
            if !health_ok(SERVER_URL) {
                if let Some(child) = spawn_server(&root) {
                    *app.state::<ServerProcess>().0.lock().unwrap() = Some(child);
                    started_here = true;
                } else {
                    eprintln!(
                        "平台启动失败，请确认 {} 下存在服务端二进制（server-rust 编译出的 edge-cluster-server）。",
                        root.display()
                    );
                }
            }

            if started_here {
                // 等平台就绪，最多 30 秒。就绪了再开窗，避免用户看到无法连接的错误页。
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut ready = false;
                while Instant::now() < deadline {
                    if health_ok(SERVER_URL) {
                        ready = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                if !ready {
                    eprintln!("平台未在 30 秒内就绪，窗口仍会打开，可能显示无法连接。");
                }
            }

            let _window = WebviewWindowBuilder::new(
                app,
                "main",
                WebviewUrl::External(url::Url::parse(SERVER_URL).expect("SERVER_URL 不是合法 URL")),
            )
            .title("Edge Cluster 控制台")
            .inner_size(1280.0, 800.0)
            .min_inner_size(900.0, 600.0)
            .on_download(|_webview, event| {
                // WebView2 默认不弹保存框，下载会静默落到默认目录甚至失败。
                // 这里在下载开始时弹系统保存对话框，让用户自己选保存位置。
                if let DownloadEvent::Requested { url, destination } = event {
                    // 文件名优先取 query 里的 filename（前端会带上真实文件名），
                    // 退而求其次用路径最后一段，最后兜底 "download"。
                    let mut filename = url
                        .query_pairs()
                        .find(|(k, _)| k == "filename")
                        .map(|(_, v)| v.into_owned())
                        .unwrap_or_default();
                    if filename.is_empty() {
                        filename = url
                            .path_segments()
                            .and_then(|mut s| s.next_back())
                            .filter(|s| !s.is_empty())
                            .unwrap_or("download")
                            .to_string();
                    }
                    let chosen = rfd::FileDialog::new()
                        .set_file_name(&filename)
                        .save_file();
                    if let Some(path) = chosen {
                        *destination = path;
                        // true = 已处理下载，WebView 不再走默认路径。
                        return true;
                    }
                    // 用户取消保存框则不改 destination，交给 WebView 的默认行为兜底。
                }
                false
            })
            .build()?;

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let RunEvent::Exit = event {
                // 收掉我们拉起来的平台子进程。用户自己先启动的（started_here=false）
                // 不会被误杀，因为那时 state 里是 None。
                if let Some(mut child) = app_handle
                    .state::<ServerProcess>()
                    .0
                    .lock()
                    .unwrap()
                    .take()
                {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
        });
}
