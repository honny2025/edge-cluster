//! 设备端文件管理（需求 §4）。沙箱内解析路径，越界一律拒绝。

use std::env;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use serde_json::{json, Value};

const MAX_WALK_ENTRIES: usize = 20000;

/// 文件操作失败。消息会原样回给平台，所以写成人能看懂的话。
#[derive(Debug, Clone)]
pub struct FileError(pub String);

impl std::fmt::Display for FileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for FileError {}

type FsResult<T> = Result<T, FileError>;

#[derive(Clone)]
struct Root {
    id: String,
    label: String,
    path: PathBuf,
    readable: bool,
    writable: bool,
}

fn home_dir() -> String {
    env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string())
}

fn candidate_roots(extra: &[(String, String, String)]) -> Vec<(String, String, String)> {
    let mut cands: Vec<(String, String, String)> = vec![
        ("home".to_string(), "Agent 主目录".to_string(), home_dir()),
        ("storage".to_string(), "内部存储".to_string(), "/sdcard".to_string()),
        ("shared".to_string(), "共享存储".to_string(), "/storage/emulated/0".to_string()),
        ("external".to_string(), "外置存储".to_string(), "/storage".to_string()),
        ("tmp".to_string(), "临时目录".to_string(), "/data/local/tmp".to_string()),
    ];
    let prefix = env::var("PREFIX").unwrap_or_default();
    if !prefix.is_empty() {
        cands.push(("prefix".to_string(), "Termux 前缀".to_string(), prefix));
    }
    cands.extend(extra.iter().cloned());
    cands
}

pub struct FileService {
    extra_roots: Vec<(String, String, String)>,
    roots_cache: Mutex<Option<Vec<Root>>>,
}

/// 磁盘空间（total, free）。跨平台：Unix 走 statvfs，Windows 走 GetDiskFreeSpaceExW，
/// 与 Python 版 `shutil.disk_usage` 语义一致（free 取 f_bavail，即非特权可用空间）。
/// 直接用 statvfs 会在 Windows 上恒为 None，而"剩余空间"是文件管理面板要展示的一格。
fn fs_space(path: &Path) -> (Option<i64>, Option<i64>) {
    match fs4::statvfs(path) {
        Ok(st) => (Some(st.total_space() as i64), Some(st.available_space() as i64)),
        Err(_) => (None, None),
    }
}

fn is_readable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            return unsafe { libc::access(c.as_ptr(), libc::R_OK) == 0 };
        }
        return false;
    }
    #[cfg(not(unix))]
    {
        fs::read_dir(path).is_ok() || fs::File::open(path).is_ok()
    }
}

fn is_writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        if let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) {
            return unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 };
        }
        return false;
    }
    #[cfg(not(unix))]
    {
        if path.is_dir() {
            let probe = path.join(".edge_wtest");
            match fs::File::create(&probe) {
                Ok(_) => {
                    let _ = fs::remove_file(&probe);
                    true
                }
                Err(_) => false,
            }
        } else {
            fs::OpenOptions::new().append(true).open(path).is_ok()
        }
    }
}

/// 词法折叠 `.` 与 `..`（不要求路径存在）。
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// 解析为绝对路径：折叠 `..`，并对已存在前缀解析软链接。
fn resolve_no_exist(p: &Path) -> PathBuf {
    let normalized = lexical_normalize(p);
    let mut existing = normalized.clone();
    let mut suffix: Vec<std::ffi::OsString> = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(n) => {
                suffix.push(n.to_os_string());
                existing = existing
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_else(|| PathBuf::from("/"));
            }
            None => break,
        }
    }
    let mut base = fs::canonicalize(&existing).unwrap_or(existing);
    for s in suffix.iter().rev() {
        base.push(s);
    }
    base
}

impl FileService {
    pub fn new(extra_roots: Vec<(String, String, String)>) -> Self {
        Self {
            extra_roots,
            roots_cache: Mutex::new(None),
        }
    }

    fn compute_roots(&self) -> Vec<Root> {
        let mut out: Vec<Root> = Vec::new();
        let mut seen: Vec<PathBuf> = Vec::new();
        for (rid, label, path) in candidate_roots(&self.extra_roots) {
            let real = match fs::canonicalize(&path) {
                Ok(p) => p,
                Err(_) => continue,
            };
            if seen.contains(&real) || !real.is_dir() {
                continue;
            }
            seen.push(real.clone());
            out.push(Root {
                id: rid,
                label,
                readable: is_readable(&real),
                writable: is_writable(&real),
                path: real,
            });
        }
        out
    }

    fn roots(&self) -> Vec<Root> {
        let mut cache = match self.roots_cache.lock() {
            Ok(c) => c,
            Err(_) => return self.compute_roots(),
        };
        if cache.is_none() {
            *cache = Some(self.compute_roots());
        }
        cache.clone().unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn invalidate(&self) {
        if let Ok(mut c) = self.roots_cache.lock() {
            *c = None;
        }
    }

    pub fn roots_json(&self) -> Vec<Value> {
        self.roots()
            .iter()
            .map(|r| {
                let (total, free) = fs_space(&r.path);
                json!({
                    "id": r.id,
                    "label": r.label,
                    "path": r.path.to_string_lossy(),
                    "readable": r.readable,
                    "writable": r.writable,
                    "total": total,
                    "free": free,
                })
            })
            .collect()
    }

    fn is_root_path(&self, real: &Path) -> bool {
        self.roots().iter().any(|r| &r.path == real)
    }

    fn in_sandbox(&self, real: &Path) -> bool {
        self.roots()
            .iter()
            .any(|r| &r.path == real || real.starts_with(&r.path))
    }

    /// 把外部传来的路径解析成沙箱内绝对路径。越界一律拒绝。
    pub fn resolve(&self, path: &str, need_write: bool) -> FsResult<PathBuf> {
        let raw = path.trim();
        if raw.is_empty() {
            return Err(FileError("路径为空".to_string()));
        }
        let p = Path::new(raw);
        let abs = if p.is_absolute() {
            p.to_path_buf()
        } else {
            let home = self
                .roots()
                .first()
                .map(|r| r.path.clone())
                .unwrap_or_else(|| PathBuf::from(home_dir()));
            home.join(raw)
        };
        let real = resolve_no_exist(&abs);
        for root in self.roots() {
            if real == root.path || real.starts_with(&root.path) {
                if need_write && !root.writable {
                    return Err(FileError(format!("根目录「{}」不可写", root.label)));
                }
                return Ok(real);
            }
        }
        Err(FileError("路径不在允许的根目录内".to_string()))
    }

    pub fn list_dir(&self, path: &str, offset: usize, limit: usize) -> FsResult<Value> {
        let target = self.resolve(path, false)?;
        if !target.exists() {
            return Err(FileError("目录不存在".to_string()));
        }
        if !target.is_dir() {
            return Err(FileError("不是一个目录".to_string()));
        }
        let mut entries: Vec<Value> = Vec::new();
        match fs::read_dir(&target) {
            Ok(it) => {
                for e in it.flatten() {
                    entries.push(entry_of(&e));
                }
            }
            Err(_) => return Err(FileError("没有权限读取该目录".to_string())),
        }
        entries.sort_by(|a, b| {
            let ad = a["is_dir"].as_bool().unwrap_or(false);
            let bd = b["is_dir"].as_bool().unwrap_or(false);
            let an = a["name"].as_str().unwrap_or("").to_lowercase();
            let bn = b["name"].as_str().unwrap_or("").to_lowercase();
            bd.cmp(&ad).then(an.cmp(&bn))
        });
        let limit = limit.clamp(1, 2000);
        let offset = offset;
        let page: Vec<Value> = entries.iter().skip(offset).take(limit).cloned().collect();
        let parent = target
            .parent()
            .map(|p| self.in_sandbox(p))
            .unwrap_or(false);
        let (total_s, free_s) = fs_space(&target);
        Ok(json!({
            "path": target.to_string_lossy(),
            "total": entries.len(),
            "offset": offset,
            "limit": limit,
            "truncated": offset + limit < entries.len(),
            "parent": if parent { target.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default() } else { String::new() },
            "space": { "total": total_s, "free": free_s },
            "entries": page,
        }))
    }

    pub fn stat(&self, path: &str) -> FsResult<Value> {
        let target = self.resolve(path, false)?;
        if !target.exists() {
            return Err(FileError("路径不存在".to_string()));
        }
        let st = fs::metadata(&target).map_err(|e| FileError(format!("读取属性失败：{e}")))?;
        let is_dir = st.is_dir();
        let is_link = fs::symlink_metadata(&target).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        let mtime = st
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
        Ok(json!({
            "path": target.to_string_lossy(),
            "name": target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| target.to_string_lossy().to_string()),
            "is_dir": is_dir,
            "is_link": is_link,
            "size": if is_dir { 0 } else { st.len() as i64 },
            "mtime": mtime,
            "mode": format!("0o{:o}", st.permissions().mode_octal()),
            "readable": is_readable(&target),
            "writable": is_writable(&target),
            "is_root": self.is_root_path(&target),
        }))
    }

    pub fn du(&self, path: &str) -> FsResult<Value> {
        let target = self.resolve(path, false)?;
        if !target.exists() {
            return Err(FileError("路径不存在".to_string()));
        }
        if target.is_file() {
            let size = target.metadata().map(|m| m.len() as i64).unwrap_or(0);
            let (total_s, free_s) = fs_space(target.parent().unwrap_or(&target));
            return Ok(json!({
                "path": target.to_string_lossy(),
                "total_size": size,
                "file_count": 1,
                "dir_count": 0,
                "truncated": false,
                "space": { "total": total_s, "free": free_s },
            }));
        }
        let mut total: i64 = 0;
        let mut files: i64 = 0;
        let mut dirs: i64 = 0;
        let mut seen: usize = 0;
        let mut truncated = false;
        du_walk(&target, &mut total, &mut files, &mut dirs, &mut seen, &mut truncated);
        let (total_s, free_s) = fs_space(&target);
        Ok(json!({
            "path": target.to_string_lossy(),
            "total_size": total,
            "file_count": files,
            "dir_count": dirs,
            "truncated": truncated,
            "space": { "total": total_s, "free": free_s },
        }))
    }

    pub fn mkdir(&self, path: &str, parents: bool) -> FsResult<Value> {
        let target = self.resolve(path, true)?;
        if target.exists() {
            if target.is_dir() {
                return Ok(json!({"path": target.to_string_lossy(), "created": false, "message": "目录已存在"}));
            }
            return Err(FileError("同名文件已存在".to_string()));
        }
        if parents {
            fs::create_dir_all(&target).map_err(|e| FileError(format!("创建目录失败：{e}")))?;
        } else {
            fs::create_dir(&target).map_err(|e| FileError(format!("创建目录失败：{e}")))?;
        }
        Ok(json!({"path": target.to_string_lossy(), "created": true}))
    }

    pub fn delete(&self, path: &str, recursive: bool) -> FsResult<Value> {
        let target = self.resolve(path, true)?;
        if !target.exists() {
            return Err(FileError("路径不存在".to_string()));
        }
        if self.is_root_path(&target) {
            return Err(FileError("拒绝删除根目录".to_string()));
        }
        let is_symlink = fs::symlink_metadata(&target).map(|m| m.file_type().is_symlink()).unwrap_or(false);
        if target.is_dir() && !is_symlink {
            if !recursive {
                let has_child = fs::read_dir(&target)
                    .map(|mut it| it.next().is_some())
                    .map_err(|e| FileError(format!("读取目录失败：{e}")))?;
                if has_child {
                    return Err(FileError("目录非空，需要显式确认递归删除".to_string()));
                }
            }
            let removed = count_entries(&target);
            fs::remove_dir_all(&target).map_err(|e| FileError(format!("删除目录失败：{e}")))?;
            return Ok(json!({"path": target.to_string_lossy(), "removed_entries": removed}));
        }
        fs::remove_file(&target).map_err(|e| FileError(format!("删除文件失败：{e}")))?;
        Ok(json!({"path": target.to_string_lossy(), "removed_entries": 1}))
    }

    pub fn transfer(&self, src: &str, dst: &str, mv: bool, overwrite: bool) -> FsResult<Value> {
        let s = self.resolve(src, mv)?;
        let d = self.resolve(dst, true)?;
        if !s.exists() {
            return Err(FileError("源路径不存在".to_string()));
        }
        if s == d {
            return Err(FileError("源与目标是同一个路径".to_string()));
        }
        if s.is_dir() && d.starts_with(&s) {
            return Err(FileError("不能把目录复制/移动到它自己的子目录里".to_string()));
        }
        if d.exists() {
            if d.is_dir() {
                return Err(FileError("目标已存在同名目录，请换一个名字".to_string()));
            }
            if !overwrite {
                return Err(FileError("目标已存在，需要显式确认覆盖".to_string()));
            }
        }
        let existed = d.exists();
        if existed {
            let _ = fs::remove_file(&d);
        }
        let result = if mv {
            if fs::rename(&s, &d).is_ok() {
                Ok(())
            } else {
                // 跨文件系统：退回复制后删除
                let r = if s.is_dir() { copy_tree(&s, &d) } else { fs::copy(&s, &d).map(|_| ()) };
                r.and_then(|_| fs::remove_dir_all(&s).or_else(|_| fs::remove_file(&s)))
            }
        } else if s.is_dir() {
            copy_tree(&s, &d)
        } else {
            fs::copy(&s, &d).map(|_| ())
        };
        result.map_err(|e| FileError(format!("{}失败：{e}", if mv { "移动" } else { "复制" })))?;
        Ok(json!({"src": s.to_string_lossy(), "dst": d.to_string_lossy(), "moved": mv, "overwritten": existed}))
    }

    /// 准备上传（设备 → 平台）。返回 (真实路径, 字节大小)。
    pub fn open_read(&self, path: &str) -> FsResult<(PathBuf, i64)> {
        let target = self.resolve(path, false)?;
        if !target.exists() {
            return Err(FileError("路径不存在".to_string()));
        }
        if target.is_dir() {
            return Err(FileError("不能直接传输目录，请先打包或逐个文件传输".to_string()));
        }
        let size = target
            .metadata()
            .map(|m| m.len() as i64)
            .map_err(|e| FileError(format!("读取文件大小失败：{e}")))?;
        Ok((target, size))
    }

    /// 准备下载（平台 → 设备）：检查父目录可写、剩余空间够。
    pub fn prepare_write(&self, path: &str, size: i64) -> FsResult<PathBuf> {
        let target = self.resolve(path, true)?;
        if target.is_dir() {
            return Err(FileError("目标是目录".to_string()));
        }
        let parent = target.parent().unwrap_or(&target);
        if !parent.exists() {
            return Err(FileError("目标目录不存在".to_string()));
        }
        if !is_writable(parent) {
            return Err(FileError("目标目录不可写".to_string()));
        }
        if size > 0 {
            if let (_, Some(free)) = fs_space(parent) {
                if free < size {
                    return Err(FileError(format!(
                        "剩余空间不足：需要 {size} 字节，可用 {free} 字节"
                    )));
                }
            }
        }
        Ok(target)
    }
}

trait ModeOctal {
    fn mode_octal(&self) -> u32;
}

#[cfg(unix)]
impl ModeOctal for std::fs::Permissions {
    fn mode_octal(&self) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        self.mode() & 0o777
    }
}

#[cfg(not(unix))]
impl ModeOctal for std::fs::Permissions {
    fn mode_octal(&self) -> u32 {
        0
    }
}

fn entry_of(e: &fs::DirEntry) -> Value {
    let name = e.file_name().to_string_lossy().to_string();
    let (is_dir, is_link) = match e.file_type() {
        Ok(t) => (t.is_dir(), t.is_symlink()),
        Err(_) => (false, false),
    };
    let (size, mtime) = match e.metadata() {
        Ok(st) => {
            let size = if is_dir { 0 } else { st.len() as i64 };
            let mtime = st
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            (Some(size), mtime)
        }
        Err(_) => (None, None),
    };
    json!({
        "name": name,
        "path": e.path().to_string_lossy(),
        "is_dir": is_dir,
        "is_link": is_link,
        "size": size,
        "mtime": mtime,
    })
}

fn du_walk(
    dir: &Path,
    total: &mut i64,
    files: &mut i64,
    dirs: &mut i64,
    seen: &mut usize,
    truncated: &mut bool,
) {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for e in entries.flatten() {
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            *dirs += 1;
            du_walk(&e.path(), total, files, dirs, seen, truncated);
        } else {
            *seen += 1;
            if *seen > MAX_WALK_ENTRIES {
                *truncated = true;
                return;
            }
            if let Ok(st) = e.metadata() {
                *total += st.len() as i64;
            } else {
                *truncated = true;
            }
            *files += 1;
        }
        if *truncated {
            return;
        }
    }
}

fn count_entries(root: &Path) -> i64 {
    let mut n = 0i64;
    count_walk(root, &mut n);
    n
}

fn count_walk(dir: &Path, n: &mut i64) {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for e in entries.flatten() {
        *n += 1;
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            count_walk(&e.path(), n);
        }
    }
}

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for e in fs::read_dir(src)? {
        let e = e?;
        let ty = e.file_type()?;
        let to = dst.join(e.file_name());
        if ty.is_symlink() {
            // 保留软链接（Python copytree(symlinks=True)）
            if let Ok(link) = fs::read_link(e.path()) {
                let _ = fs::remove_file(&to);
                #[cfg(unix)]
                std::os::unix::fs::symlink(link, &to)?;
                #[cfg(not(unix))]
                let _ = link;
            }
        } else if ty.is_dir() {
            copy_tree(&e.path(), &to)?;
        } else {
            fs::copy(e.path(), &to)?;
        }
    }
    Ok(())
}
