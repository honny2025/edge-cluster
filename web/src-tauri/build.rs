// Tauri 构建脚本。作用：在编译期把 tauri.conf.json 里声明的资源、
// 图标、Windows 资源清单等准备好，供 tauri::generate_context!() 使用。
fn main() {
    tauri_build::build()
}
