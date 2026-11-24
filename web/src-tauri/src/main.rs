// 桌面壳入口。不处理任何业务，只把窗口交给 lib.rs。
//
// windows_subsystem = "windows"：发布版不弹黑色控制台窗口（双击就一个干净的窗口）。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    edge_cluster_desktop_lib::run();
}
