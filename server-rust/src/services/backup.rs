//! 备份与恢复（A4）。与 Python services/backup.py 对齐。
//!
//! 为什么不能直接拷 edge.db：WAL 模式下最近的写入还在 -wal 里没合并进主文件，
//! 直接复制会丢最后一截。所以备份用 `VACUUM INTO` 得到一份一致快照。
//!
//! 恢复是破坏性操作，四道闸：显式 confirm；先把当前库安全备份并验证可读；
//! 上传的包必须是合法 zip + 含 edge.db + SQLite 魔数；只有最后一步才动数据。

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Map, Value};

use crate::config;
use crate::time;

pub const BACKUP_KIND: &str = "edge-cluster-backup";
pub const BACKUP_FORMAT: i64 = 1;
pub const DB_MEMBER: &str = "edge.db";
pub const MANIFEST_MEMBER: &str = "manifest.json";
const MODELS_MEMBER_PREFIX: &str = "models/";
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\x00";

/// 进 manifest 的统计表。顺序按"用户最关心"排。注意是 `deploy_tasks`
///（Python 版这里写的是 `deployments`，但那是一处会静默跳过计数的笔误）。
const COUNT_TABLES: &[&str] = &[
    "nodes",
    "models",
    "deploy_tasks",
    "services",
    "metric_samples",
    "metric_rollups",
    "audit_logs",
    "file_transfers",
];

fn app_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn sha256_file(path: &Path) -> String {
    use sha2::{Digest, Sha256};
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = match file.read(&mut buf) {
            Ok(n) => n,
            Err(_) => return String::new(),
        };
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    hex::encode(hasher.finalize())
}

/// 用 SQLite 自己的一致性快照（VACUUM INTO），而不是复制文件。
fn snapshot_db(dest: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(dest);
    let conn = Connection::open(config::get().db_file()).map_err(|e| e.to_string())?;
    let res = conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().to_string()]);
    drop(conn);
    res.map(|_| ()).map_err(|e| e.to_string())
}

fn table_counts(db_file: &Path) -> Value {
    let mut out = Map::new();
    let conn = match Connection::open(db_file) {
        Ok(c) => c,
        Err(_) => return Value::Object(out),
    };
    for t in COUNT_TABLES {
        let n: Option<i64> = conn
            .query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0))
            .ok();
        if let Some(n) = n {
            out.insert((*t).to_string(), json!(n));
        }
    }
    Value::Object(out)
}

fn model_entries(db_file: &Path) -> Vec<Value> {
    let mut out = Vec::new();
    let conn = match Connection::open(db_file) {
        Ok(c) => c,
        Err(_) => return out,
    };
    let mut stmt = match conn.prepare(
        "SELECT model_id, name, version, file_name, file_size, sha256, storage_path \
         FROM models ORDER BY name, version",
    ) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let iter = match stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, i64>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
        ))
    }) {
        Ok(it) => it,
        Err(_) => return out,
    };
    let models_path = config::get().models_path();
    for row in iter.flatten() {
        let (model_id, name, version, file_name, file_size, sha256, storage_path) = row;
        let storage = storage_path.trim().to_string();
        let src = if storage.is_empty() {
            None
        } else {
            Some(models_path.join(&storage))
        };
        let file_present = src.as_ref().map(|p| p.is_file()).unwrap_or(false);
        out.push(json!({
            "model_id": model_id,
            "name": name,
            "version": version,
            "file_name": file_name,
            "file_size": file_size,
            "sha256": sha256,
            "storage_path": storage,
            "file_present": file_present,
        }));
    }
    out
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk_files(&p, out);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
}

fn open_zip(zip_path: &Path) -> Result<zip::ZipArchive<std::fs::File>, String> {
    let file = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(file).map_err(|e| format!("不是合法的 zip 文件: {e}"))
}

fn read_manifest(zf: &mut zip::ZipArchive<std::fs::File>) -> Result<Value, String> {
    let mut member = zf
        .by_name(MANIFEST_MEMBER)
        .map_err(|_| "这不是 edge-cluster 备份包：缺少 manifest.json".to_string())?;
    let mut raw = Vec::new();
    member
        .read_to_end(&mut raw)
        .map_err(|e| format!("读取 manifest 失败: {e}"))?;
    let text = String::from_utf8(raw).map_err(|_| "manifest.json 不是 UTF-8 文本".to_string())?;
    let data: Value =
        serde_json::from_str(&text).map_err(|e| format!("manifest.json 损坏，无法解析: {e}"))?;
    if data.get("kind").and_then(|v| v.as_str()) != Some(BACKUP_KIND) {
        return Err("这不是 edge-cluster 备份包：manifest 里的 kind 对不上".to_string());
    }
    let fmt = data.get("format").and_then(|v| v.as_i64()).unwrap_or(-1);
    if fmt != BACKUP_FORMAT {
        return Err(format!(
            "备份包格式为 {fmt}，本版本只认识 {BACKUP_FORMAT}。请用与备份时相同或更新的平台版本恢复。"
        ));
    }
    Ok(data)
}

// --------------------------------------------------------------------- 备份

/// 打一个备份 zip，返回文件路径。`include_models=true` 会把 models/ 整个塞进去。
pub fn create_backup(include_models: bool, out_dir: Option<&Path>) -> Result<PathBuf, String> {
    config::get().ensure_dirs().map_err(|e| e.to_string())?;
    let stamp = time::now().format("%Y%m%d-%H%M%S").to_string();
    let tmp = std::env::temp_dir().join(format!("edge-backup-{}", crate::util::new_uuid_hex()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;

    let result = (|| -> Result<PathBuf, String> {
        let snap = tmp.join(DB_MEMBER);
        snapshot_db(&snap)?;
        let db_size = snap.metadata().map(|m| m.len()).unwrap_or(0);
        let db_sha = sha256_file(&snap);

        let manifest = json!({
            "kind": BACKUP_KIND,
            "format": BACKUP_FORMAT,
            "app_version": app_version(),
            "created_at": time::format_iso(&time::now()),
            "created_by": "platform",
            "db": { "file": DB_MEMBER, "size": db_size, "sha256": db_sha },
            "tables": table_counts(&snap),
            "models": model_entries(&snap),
            "includes_models": include_models,
            "note": "这是 edge-cluster 平台的备份。恢复用 POST /api/v1/system/backup/restore?confirm=true 上传本文件。",
        });

        let target_dir = match out_dir {
            Some(d) => d.to_path_buf(),
            None => config::get()
                .db_file()
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("backups"),
        };
        std::fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
        let out = target_dir.join(format!("edge-cluster-backup-{stamp}.zip"));

        let file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        let mut zf = zip::ZipWriter::new(file);
        let options = || {
            zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
        };

        zf.start_file(DB_MEMBER, options()).map_err(|e| e.to_string())?;
        zf.write_all(&std::fs::read(&snap).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;

        if include_models {
            let mp = config::get().models_path();
            if mp.is_dir() {
                let mut files = Vec::new();
                walk_files(&mp, &mut files);
                files.sort();
                for p in files {
                    let rel = p
                        .strip_prefix(&mp)
                        .map_err(|e| e.to_string())?
                        .to_string_lossy()
                        .replace('\\', "/");
                    let arcname = format!("{MODELS_MEMBER_PREFIX}{rel}");
                    zf.start_file(arcname, options()).map_err(|e| e.to_string())?;
                    zf.write_all(&std::fs::read(&p).map_err(|e| e.to_string())?)
                        .map_err(|e| e.to_string())?;
                }
            }
        }

        // manifest 最后写：先有数据、后有索引。
        let manifest_str = serde_json::to_string_pretty(&manifest).map_err(|e| e.to_string())?;
        zf.start_file(MANIFEST_MEMBER, options()).map_err(|e| e.to_string())?;
        zf.write_all(manifest_str.as_bytes()).map_err(|e| e.to_string())?;
        zf.finish().map_err(|e| e.to_string())?;
        Ok(out)
    })();

    let _ = std::fs::remove_dir_all(&tmp);
    result
}

// --------------------------------------------------------------------- 校验

/// 只读校验。返回 {ok, problems, manifest, db_size, db_sha256, sha256_matches,
/// includes_models, model_files_in_package, current_app_version}。
pub fn inspect_backup(zip_path: &Path) -> Value {
    let mut problems: Vec<String> = Vec::new();
    let mut manifest: Value = Value::Null;
    let mut db_size: i64 = 0;
    let mut db_sha256 = String::new();
    let mut sha256_matches: Option<bool> = None;
    let mut includes_models = false;
    let mut model_files_in_package: i64 = 0;

    let mut zf = match open_zip(zip_path) {
        Ok(z) => z,
        Err(e) => {
            problems.push(e);
            return build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package);
        }
    };

    // 读 manifest
    manifest = match read_manifest(&mut zf) {
        Ok(m) => m,
        Err(e) => {
            problems.push(e);
            return build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package);
        }
    };

    let names: Vec<String> = zf.file_names().map(|s| s.to_string()).collect();
    if !names.iter().any(|n| n == DB_MEMBER) {
        problems.push("包里没有 edge.db —— 备份不完整，不能用于恢复".to_string());
        return build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package);
    }

    let info_size = zf
        .by_name(DB_MEMBER)
        .map(|m| m.size())
        .unwrap_or(0);
    db_size = info_size as i64;

    // 先看魔数再算 sha256
    {
        let mut member = match zf.by_name(DB_MEMBER) {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("读取 edge.db 失败: {e}"));
                return build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package);
            }
        };
        let mut head = vec![0u8; SQLITE_MAGIC.len()];
        let mut got = 0;
        while got < head.len() {
            match member.read(&mut head[got..]) {
                Ok(0) => break,
                Ok(n) => got += n,
                Err(_) => break,
            }
        }
        if &head[..got] != SQLITE_MAGIC {
            problems.push("edge.db 不是合法的 SQLite 文件（文件头对不上）".to_string());
            return build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package);
        }
    }

    // 完整 sha256
    {
        use sha2::{Digest, Sha256};
        let mut member = match zf.by_name(DB_MEMBER) {
            Ok(m) => m,
            Err(e) => {
                problems.push(format!("读取 edge.db 失败: {e}"));
                return build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package);
            }
        };
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            match member.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => hasher.update(&buf[..n]),
                Err(_) => break,
            }
        }
        db_sha256 = hex::encode(hasher.finalize());
    }

    let expect = manifest
        .get("db")
        .and_then(|d| d.get("sha256"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !expect.is_empty() {
        sha256_matches = Some(expect == db_sha256);
        if expect != db_sha256 {
            problems.push("edge.db 的 sha256 与 manifest 记录不一致 —— 文件在传输中损坏了".to_string());
        }
    } else {
        problems.push("manifest 里没有记录 sha256，无法校验完整性（老版本打的包）".to_string());
    }

    includes_models = manifest
        .get("includes_models")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    model_files_in_package = names
        .iter()
        .filter(|n| n.starts_with(MODELS_MEMBER_PREFIX) && !n.ends_with('/'))
        .count() as i64;

    // 用临时副本真开一次库，确认能打开、表也在
    let probe_dir = std::env::temp_dir().join(format!("edge-verify-{}", crate::util::new_uuid_hex()));
    if std::fs::create_dir_all(&probe_dir).is_ok() {
        let probe = probe_dir.join(DB_MEMBER);
        let copied = {
            let mut member = zf.by_name(DB_MEMBER).ok();
            let mut dst = std::fs::File::create(&probe).ok();
            match (member.as_mut(), dst.as_mut()) {
                (Some(m), Some(d)) => std::io::copy(m, d).is_ok(),
                _ => false,
            }
        };
        if copied {
            if let Ok(conn) = Connection::open_with_flags(&probe, OpenFlags::SQLITE_OPEN_READ_ONLY) {
                let integrity: Option<String> = conn
                    .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                    .ok();
                match integrity.as_deref() {
                    Some("ok") => {}
                    Some(other) => problems.push(format!("SQLite 完整性检查未通过: {other}")),
                    None => problems.push("SQLite 完整性检查执行失败".to_string()),
                }
                let mut tables = std::collections::HashSet::new();
                if let Ok(mut stmt) = conn.prepare("SELECT name FROM sqlite_master WHERE type='table'") {
                    if let Ok(iter) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                        for t in iter.flatten() {
                            tables.insert(t);
                        }
                    }
                }
                let required = ["nodes", "models", "audit_logs"];
                let missing: Vec<&str> = required.iter().filter(|t| !tables.contains(**t)).copied().collect();
                if !missing.is_empty() {
                    problems.push(format!(
                        "库结构不对，缺表: {} —— 这不像本平台的备份",
                        missing.join(", ")
                    ));
                }
            } else {
                problems.push("SQLite 打不开这个库".to_string());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&probe_dir);

    build_inspect(&problems, manifest, db_size, db_sha256, sha256_matches, includes_models, model_files_in_package)
}

#[allow(clippy::too_many_arguments)]
fn build_inspect(
    problems: &[String],
    manifest: Value,
    db_size: i64,
    db_sha256: String,
    sha256_matches: Option<bool>,
    includes_models: bool,
    model_files_in_package: i64,
) -> Value {
    json!({
        "ok": problems.is_empty(),
        "problems": problems,
        "manifest": manifest,
        "db_size": db_size,
        "db_sha256": db_sha256,
        "sha256_matches": sha256_matches,
        "includes_models": includes_models,
        "model_files_in_package": model_files_in_package,
        "current_app_version": app_version(),
    })
}

// --------------------------------------------------------------------- 恢复

/// 从备份包恢复。破坏性操作。`confirm=false` 时只做校验并说明后果。
pub fn restore_backup(zip_path: &Path, confirm: bool) -> Value {
    if !confirm {
        return json!({
            "ok": false,
            "dry_run": true,
            "message": "恢复会**替换当前的全部数据**（节点、模型记录、部署历史、审计记录）。确认无误请带 confirm=true 重新调用。",
            "preview": inspect_backup(zip_path),
        });
    }

    let check = inspect_backup(zip_path);
    if check.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        return json!({
            "ok": false, "dry_run": false,
            "message": "备份包校验未通过，未做任何改动",
            "preview": check,
        });
    }

    let db_file = config::get().db_file();
    let stamp = time::now().format("%Y%m%d-%H%M%S").to_string();
    let safety = db_file.with_file_name(format!("{}.bak-{stamp}", db_file.file_name().unwrap_or_default().to_string_lossy()));

    // ① 先把现有库安全备份走，并验证这份备份真能读。
    if db_file.exists() {
        if let Err(e) = snapshot_db(&safety) {
            let _ = std::fs::remove_file(&safety);
            return json!({
                "ok": false, "dry_run": false,
                "message": format!("当前数据的安全备份做不出来（{e}），为保证不丢数据，已中止恢复。"),
                "preview": check,
            });
        }
        let readable = Connection::open(&safety)
            .map(|c| c.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0)).is_ok())
            .unwrap_or(false);
        if !readable {
            let _ = std::fs::remove_file(&safety);
            return json!({
                "ok": false, "dry_run": false,
                "message": "当前数据的安全备份无法读取，为保证不丢数据，已中止恢复。",
                "preview": check,
            });
        }
    }

    // ② 从包里解出库到临时文件
    let tmp_dir = std::env::temp_dir().join(format!("edge-restore-{}", crate::util::new_uuid_hex()));
    let staged = tmp_dir.join(DB_MEMBER);
    let extract_ok = (|| -> Result<(), String> {
        std::fs::create_dir_all(&tmp_dir).map_err(|e| e.to_string())?;
        let mut zf = open_zip(zip_path)?;
        let mut src = zf.by_name(DB_MEMBER).map_err(|e| e.to_string())?;
        let mut dst = std::fs::File::create(&staged).map_err(|e| e.to_string())?;
        std::io::copy(&mut src, &mut dst).map_err(|e| e.to_string())?;
        Ok(())
    })();

    if let Err(e) = extract_ok {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return json!({
            "ok": false, "dry_run": false,
            "message": format!("从备份包解出数据库失败（{e}），未做任何改动。"),
            "preview": check,
        });
    }

    // ③ 用 SQLite 在线备份 API 把快照覆盖进现有库（不 rename 文件，避开 Windows
    //    上"文件被连接占用时不可替换"的问题）。恢复后建议重启平台。
    let apply_ok = (|| -> Result<(), String> {
        let src = Connection::open(&staged).map_err(|e| e.to_string())?;
        let mut dst = Connection::open(&db_file).map_err(|e| e.to_string())?;
        let backup = rusqlite::backup::Backup::new(&src, &mut dst).map_err(|e| e.to_string())?;
        backup
            .run_to_completion(1024, std::time::Duration::from_millis(0), None)
            .map_err(|e| e.to_string())?;
        drop(backup);
        Ok(())
    })();

    let _ = std::fs::remove_dir_all(&tmp_dir);

    if let Err(e) = apply_ok {
        return json!({
            "ok": false, "dry_run": false,
            "message": format!("写入恢复数据失败（{e}）。旧数据的安全备份仍在 {}。", safety.display()),
            "safety_backup": safety.to_string_lossy(),
            "preview": check,
        });
    }

    // ④ 清掉 -wal / -shm，避免旧 WAL 往新库上重放。
    for suffix in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{}", db_file.to_string_lossy(), suffix));
        let _ = std::fs::remove_file(p);
    }

    // ⑤ 跑一遍建表/补列，让更老版本打出来的包也能用。
    if let Ok(conn) = Connection::open(&db_file) {
        let _ = crate::db::init_db(&conn);
    }

    let counts = table_counts(&db_file);
    json!({
        "ok": true,
        "dry_run": false,
        "message": "恢复完成。**建议重启平台**，让所有后台任务用上新数据。",
        "restored_from": zip_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        "safety_backup": safety.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
        "tables": counts,
        "manifest": check.get("manifest").cloned().unwrap_or(Value::Null),
        "includes_models": check.get("includes_models").and_then(|v| v.as_bool()).unwrap_or(false),
        "model_files_in_package": check.get("model_files_in_package").and_then(|v| v.as_i64()).unwrap_or(0),
    })
}

// --------------------------------------------------------------------- 现状

/// 库与备份目录的现状。
pub fn backup_status() -> Value {
    let db_file = config::get().db_file();
    let backup_dir = db_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups");
    let mut files = Vec::new();
    if backup_dir.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&backup_dir)
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        entries.sort_by(|a, b| b.cmp(a));
        for p in entries.into_iter().take(20) {
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if !name.starts_with("edge-cluster-backup-") || !name.ends_with(".zip") {
                continue;
            }
            if let Ok(st) = p.metadata() {
                let modified = st
                    .modified()
                    .ok()
                    .map(|m| {
                        let dt: chrono::DateTime<chrono::Utc> = m.into();
                        dt.format("%Y-%m-%dT%H:%M:%S").to_string()
                    })
                    .unwrap_or_default();
                files.push(json!({ "name": name, "size": st.len(), "modified": modified }));
            }
        }
    }
    json!({
        "db_file": db_file.to_string_lossy(),
        "db_size": db_file.metadata().map(|m| m.len()).unwrap_or(0),
        "wal_present": PathBuf::from(format!("{}-wal", db_file.to_string_lossy())).exists(),
        "tables": if db_file.exists() { table_counts(&db_file) } else { json!({}) },
        "backup_dir": backup_dir.to_string_lossy(),
        "backup_count": files.len(),
        "backups": files,
        "models_dir": config::get().models_path().to_string_lossy(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{init_global, Config, DataConfig};

    struct Results {
        failures: Vec<String>,
    }
    impl Results {
        fn new() -> Self {
            Self { failures: vec![] }
        }
        fn check(&mut self, name: &str, ok: bool, detail: impl AsRef<str>) {
            if !ok {
                self.failures.push(format!("{name} — {}", detail.as_ref()));
            }
        }
        fn finish(self) {
            assert!(
                self.failures.is_empty(),
                "\n备份验收失败 {} 项：\n{}",
                self.failures.len(),
                self.failures.join("\n")
            );
        }
    }

    /// 建一个指向临时目录的全局配置，返回 (db 路径, models 目录, 根目录)。
    fn setup() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("edge-backup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let data = root.join("data");
        let models = root.join("models");
        let logs = root.join("logs");
        for d in [&data, &models, &logs] {
            std::fs::create_dir_all(d).unwrap();
        }
        let db = data.join("edge.db");
        init_global(Config {
            data: DataConfig {
                db_path: db.to_string_lossy().to_string(),
                models_dir: models.to_string_lossy().to_string(),
                logs_dir: logs.to_string_lossy().to_string(),
            },
            ..Config::default()
        });
        (db, models, root)
    }

    fn make_zip(path: &Path, members: &[(&str, Vec<u8>)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut zf = zip::ZipWriter::new(f);
        let opts = zip::write::FileOptions::default();
        for (name, data) in members {
            zf.start_file(*name, opts).unwrap();
            zf.write_all(data).unwrap();
        }
        zf.finish().unwrap();
    }

    fn zip_member_bytes(zip: &Path, name: &str) -> Option<Vec<u8>> {
        let f = std::fs::File::open(zip).ok()?;
        let mut zf = zip::ZipArchive::new(f).ok()?;
        let mut m = zf.by_name(name).ok()?;
        let mut out = Vec::new();
        m.read_to_end(&mut out).ok()?;
        Some(out)
    }

    /// 造一份最小 manifest，用于打「坏包」（kind 对不上 / format 不认）。
    fn manifest_bytes(kind: &str, format: i64) -> Vec<u8> {
        serde_json::json!({
            "kind": kind, "format": format, "app_version": "test",
            "created_at": "2026-01-01T00:00:00",
            "db": {"file": DB_MEMBER, "size": 0, "sha256": ""},
            "tables": {}, "models": [], "includes_models": false,
        })
        .to_string()
        .into_bytes()
    }

    fn problems(v: &Value) -> Vec<String> {
        v.get("problems")
            .and_then(|p| p.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default()
    }

    fn node_ids(db: &Path) -> Vec<String> {
        let conn = Connection::open(db).unwrap();
        let mut stmt = conn.prepare("SELECT node_id FROM nodes ORDER BY node_id").unwrap();
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
        rows.flatten().collect()
    }

    #[test]
    fn backup_roundtrip() {
        let _guard = crate::config::CONFIG_TEST_LOCK.lock().unwrap();
        let mut r = Results::new();
        let (db, models_dir, root) = setup();

        // 建库 + 造数据：old-01 / old-02 两个节点 + 一个模型 + 一个模型权重文件
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        crate::db::init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO nodes (node_id, name, status, created_at, updated_at) \
             VALUES ('old-01','旧节点一','online','2026-01-01T00:00:00','2026-01-01T00:00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO nodes (node_id, name, status, created_at, updated_at) \
             VALUES ('old-02','旧节点二','offline','2026-01-01T00:00:00','2026-01-01T00:00:00')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO models (model_id, name, version, file_name, file_size, sha256, storage_path, uploaded_at) \
             VALUES ('m-old','旧模型','v1','old.bin',11, ?1, 'm-old-old.bin','2026-01-01T00:00:00')",
            ["a".repeat(64)],
        )
        .unwrap();
        drop(conn); // 落盘（WAL 里还有最近提交）

        std::fs::write(models_dir.join("m-old-old.bin"), b"old-model!!").unwrap();

        // ---------- A/B：备份 + manifest 属实 ----------
        let bkp = create_backup(false, None).expect("打备份失败");

        // A：把备份里的 edge.db 单独解到一个干净目录，不给 -wal/-shm 也能读全
        let lone = root.join("lone");
        std::fs::create_dir_all(&lone).unwrap();
        let lone_db = lone.join(DB_MEMBER);
        std::fs::write(&lone_db, zip_member_bytes(&bkp, DB_MEMBER).expect("包里没有 edge.db")).unwrap();
        {
            let c = Connection::open_with_flags(&lone_db, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let got: Vec<String> = {
                let mut stmt = c.prepare("SELECT node_id FROM nodes ORDER BY node_id").unwrap();
                let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
                rows.flatten().collect()
            };
            r.check(
                "A 单独拿出 edge.db（不给 -wal）也能读到全部数据",
                got == ["old-01", "old-02"],
                format!("{got:?}"),
            );
        }

        // B：manifest 属实
        let man: Value = serde_json::from_slice(
            &zip_member_bytes(&bkp, MANIFEST_MEMBER).expect("包里没有 manifest.json"),
        )
        .unwrap();
        r.check(
            "B manifest 记了 kind/format",
            man["kind"].as_str() == Some(BACKUP_KIND) && man["format"].as_i64() == Some(BACKUP_FORMAT),
            format!("kind={:?} format={:?}", man["kind"], man["format"]),
        );
        r.check(
            "B db.sha256 与包内 edge.db 实际一致",
            man["db"]["sha256"].as_str() == Some(sha256_file(&lone_db).as_str()),
            format!("{:?}", man["db"]["sha256"]),
        );
        r.check(
            "B tables 行数与真实库一致",
            man["tables"]["nodes"].as_i64() == Some(2) && man["tables"]["models"].as_i64() == Some(1),
            format!("{:?}", man["tables"]),
        );
        r.check(
            "B 模型清单如实标注 file_present=true",
            man["models"][0]["file_present"].as_bool() == Some(true),
            format!("{:?}", man["models"][0]["file_present"]),
        );
        r.check(
            "B 默认不含模型权重",
            man["includes_models"].as_bool() == Some(false),
            format!("includes_models={:?}", man["includes_models"]),
        );

        // ---------- C：坏包必须被识别 ----------
        let bad = root.join("bad");
        std::fs::create_dir_all(&bad).unwrap();

        let not_zip = bad.join("not-a-zip.zip");
        std::fs::write(&not_zip, b"this is definitely not a zip file").unwrap();
        let v = inspect_backup(&not_zip);
        r.check("C 非 zip → ok=false", v["ok"].as_bool() == Some(false), problems(&v).first().cloned().unwrap_or_default());

        let no_manifest = bad.join("no-manifest.zip");
        make_zip(&no_manifest, &[(DB_MEMBER, std::fs::read(&lone_db).unwrap())]);
        let v = inspect_backup(&no_manifest);
        r.check(
            "C 缺 manifest → 报不是本平台备份",
            v["ok"].as_bool() == Some(false) && problems(&v).iter().any(|p| p.contains("manifest")),
            problems(&v).first().cloned().unwrap_or_default(),
        );

        let wrong_kind = bad.join("wrong-kind.zip");
        make_zip(&wrong_kind, &[
            (MANIFEST_MEMBER, manifest_bytes("someone-elses-backup", BACKUP_FORMAT)),
            (DB_MEMBER, std::fs::read(&lone_db).unwrap()),
        ]);
        let v = inspect_backup(&wrong_kind);
        r.check(
            "C kind 对不上 → 拒绝",
            v["ok"].as_bool() == Some(false) && problems(&v).iter().any(|p| p.contains("kind")),
            problems(&v).first().cloned().unwrap_or_default(),
        );

        let future_fmt = bad.join("future.zip");
        make_zip(&future_fmt, &[
            (MANIFEST_MEMBER, manifest_bytes(BACKUP_KIND, 99)),
            (DB_MEMBER, std::fs::read(&lone_db).unwrap()),
        ]);
        let v = inspect_backup(&future_fmt);
        r.check(
            "C format 不认 → 明确拒绝",
            v["ok"].as_bool() == Some(false) && problems(&v).iter().any(|p| p.contains("格式")),
            problems(&v).first().cloned().unwrap_or_default(),
        );

        let no_db = bad.join("no-db.zip");
        make_zip(&no_db, &[(MANIFEST_MEMBER, manifest_bytes(BACKUP_KIND, BACKUP_FORMAT))]);
        let v = inspect_backup(&no_db);
        r.check(
            "C 包里没有 edge.db → 拒绝",
            v["ok"].as_bool() == Some(false) && problems(&v).iter().any(|p| p.contains("edge.db")),
            problems(&v).first().cloned().unwrap_or_default(),
        );

        let not_sqlite = bad.join("not-sqlite.zip");
        make_zip(&not_sqlite, &[
            (MANIFEST_MEMBER, manifest_bytes(BACKUP_KIND, BACKUP_FORMAT)),
            (DB_MEMBER, b"PK\x03\x04 not a database at all".repeat(100).to_vec()),
        ]);
        let v = inspect_backup(&not_sqlite);
        r.check(
            "C edge.db 不是 SQLite（魔数不对）→ 拒绝",
            v["ok"].as_bool() == Some(false) && problems(&v).iter().any(|p| p.contains("SQLite") || p.contains("文件头")),
            problems(&v).first().cloned().unwrap_or_default(),
        );

        // 篡改一个字节 → sha256 对不上
        let mut tampered_db = std::fs::read(&lone_db).unwrap();
        *tampered_db.last_mut().unwrap() ^= 0xFF;
        let tampered = bad.join("tampered.zip");
        make_zip(&tampered, &[
            (MANIFEST_MEMBER, zip_member_bytes(&bkp, MANIFEST_MEMBER).unwrap()),
            (DB_MEMBER, tampered_db),
        ]);
        let v = inspect_backup(&tampered);
        r.check(
            "C 库被改一个字节 → sha256 抓得住",
            v["ok"].as_bool() == Some(false)
                && v["sha256_matches"].as_bool() == Some(false)
                && problems(&v).iter().any(|p| p.contains("sha256")),
            problems(&v).first().cloned().unwrap_or_default(),
        );

        let v = inspect_backup(&bkp);
        r.check("C 自己打的包必须校验通过", v["ok"].as_bool() == Some(true), problems(&v).join("; "));

        // ---------- D：dry-run 一个字节都不动 ----------
        let before_sha = sha256_file(&db);
        let dry = restore_backup(&bkp, false);
        r.check(
            "D confirm=false → dry_run、ok=false",
            dry["dry_run"].as_bool() == Some(true) && dry["ok"].as_bool() == Some(false),
            format!("{:?}", dry.get("message")),
        );
        r.check("D 库文件字节完全没变", sha256_file(&db) == before_sha, "sha 变了");

        // ---------- E/F/G：真恢复，换一套数据再换回来 ----------
        let conn = Connection::open(&db).unwrap();
        conn.execute("DELETE FROM models", []).unwrap();
        conn.execute("DELETE FROM nodes", []).unwrap();
        conn.execute(
            "INSERT INTO nodes (node_id, name, status, created_at, updated_at) \
             VALUES ('new-99','恢复后不该存在的节点','online','2026-01-01T00:00:00','2026-01-01T00:00:00')",
            [],
        )
        .unwrap();
        drop(conn);
        r.check("已把数据换成另一套", node_ids(&db) == ["new-99"], format!("{:?}", node_ids(&db)));

        let res = restore_backup(&bkp, true);
        r.check("E 恢复返回 ok=true", res["ok"].as_bool() == Some(true), format!("{:?}", res.get("message")));

        let ids = node_ids(&db);
        r.check("E 数据换回了备份里的那份", ids == ["old-01", "old-02"], format!("{ids:?}"));
        r.check("G 恢复后旧 WAL 没有把 new-99 带回来", !ids.iter().any(|x| x == "new-99"), format!("{ids:?}"));

        // F：恢复前自动留的安全备份存在、且可读、里面正是 new-99
        let safety_name = res["safety_backup"].as_str().unwrap_or("").to_string();
        r.check("F 恢复前留了安全备份", !safety_name.is_empty(), safety_name.clone());
        if !safety_name.is_empty() {
            let safety_path = db.with_file_name(&safety_name);
            r.check("F 安全备份文件存在", safety_path.is_file(), safety_path.to_string_lossy().to_string());
            let cnt: i64 = Connection::open(&safety_path)
                .map(|c| c.query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get(0)).unwrap_or(-1))
                .unwrap_or(-1);
            r.check("F 安全备份可读，里面正是 new-99", cnt == 1, format!("{cnt} 行"));
        }

        // ---------- H：恢复完还能用 ----------
        let conn = Connection::open(&db).unwrap();
        crate::db::init_db(&conn).unwrap();
        drop(conn);
        r.check("H init_db 在恢复后的库上跑得通", node_ids(&db) == ["old-01", "old-02"], format!("{:?}", node_ids(&db)));

        let again = create_backup(false, None).expect("再打备份失败");
        let v = inspect_backup(&again);
        r.check("H 恢复完还能再打一份备份", v["ok"].as_bool() == Some(true), problems(&v).join("; "));

        // ---------- I：include_models 开关 ----------
        let with_models = create_backup(true, None).expect("打带模型的备份失败");
        let man: Value = serde_json::from_slice(
            &zip_member_bytes(&with_models, MANIFEST_MEMBER).unwrap(),
        )
        .unwrap();
        r.check(
            "I include_models=true 时权重进了包",
            man["includes_models"].as_bool() == Some(true),
            "includes_models != true",
        );
        let v = inspect_backup(&with_models);
        r.check(
            "I 带模型的包也能通过校验",
            v["ok"].as_bool() == Some(true),
            problems(&v).join("; "),
        );
        r.check(
            "I 数出包内模型文件数 ≥1",
            v["model_files_in_package"].as_i64().unwrap_or(0) >= 1,
            format!("{:?}", v["model_files_in_package"]),
        );

        // ---------- J：现状汇报 ----------
        let st = backup_status();
        r.check(
            "J backup_status 带库大小、表行数、备份目录",
            st["db_size"].as_i64().unwrap_or(0) > 0
                && st["tables"]["nodes"].as_i64() == Some(2)
                && st["backup_count"].as_i64().unwrap_or(0) >= 1,
            format!("db={:?} nodes={:?} backups={:?}", st["db_size"], st["tables"]["nodes"], st["backup_count"]),
        );
        r.check(
            "J wal_present 是布尔",
            st["wal_present"].is_boolean(),
            format!("{:?}", st["wal_present"]),
        );

        let _ = std::fs::remove_dir_all(&root);
        r.finish();
    }
}
