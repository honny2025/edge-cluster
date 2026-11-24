//! 部署回滚（A1）—— 部署失败时，把设备还原到"动手之前"的样子。
//!
//! 三条纪律：拿不到后悔药就不动手；回滚之后必须验证；只碰自己记下的路径。

use std::fs;
use std::path::{Path, PathBuf};

const MANIFEST_NAME: &str = "journal.json";

/// 留不下后悔药。调用方必须中止。
#[derive(Debug, Clone)]
pub struct RollbackError(pub String);

impl std::fmt::Display for RollbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for RollbackError {}

pub struct InstallJournal {
    dir: PathBuf,
    saved: Vec<(PathBuf, PathBuf)>, // (挪到哪, 原本在哪)
    created: Vec<PathBuf>,
    prepared: bool,
    failed_outcome: Option<(bool, String)>,
}

impl InstallJournal {
    pub fn new(base_dir: &Path, task_id: Option<&str>) -> Self {
        let safe: String = task_id
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        let safe = if safe.is_empty() { "untagged" } else { &safe };
        Self {
            dir: base_dir.join(safe),
            saved: Vec::new(),
            created: Vec::new(),
            prepared: false,
            failed_outcome: None,
        }
    }

    /// 建立回滚点。返回说明（正常为空串）。遗留清单先还原再开工。
    pub fn prepare(&mut self) -> Result<String, RollbackError> {
        let stale = self.read_manifest();
        let mut note = String::new();
        if let Some(stale) = stale {
            let saved = pairs(stale.get("saved"));
            let created: Vec<PathBuf> = stale
                .get("created")
                .and_then(|c| c.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .filter(|s| !s.trim().is_empty())
                        .map(PathBuf::from)
                        .collect()
                })
                .unwrap_or_default();
            let (ok, detail) = Self::apply(&saved, &created);
            if !ok {
                return Err(RollbackError(format!(
                    "上一轮部署遗留的回滚日志还原失败（{detail}）。这些文件还在 {}，没有被删掉，请人工确认后再试。",
                    self.dir.display()
                )));
            }
            note = format!("已还原上一轮遗留的回滚点（{detail}）");
            let _ = fs::remove_dir_all(&self.dir);
        }
        fs::create_dir_all(&self.dir)
            .map_err(|e| RollbackError(format!("无法创建回滚日志目录 {}：{e}", self.dir.display())))?;
        self.prepared = true;
        self.flush();
        Ok(note)
    }

    /// 记下"这个文件即将被改动"。必须在改动之前调用。
    pub fn capture(&mut self, path: &Path) -> Result<(), RollbackError> {
        if !self.prepared {
            return Err(RollbackError("回滚点尚未建立，不能动设备上的文件".to_string()));
        }
        let p = path;
        if self.saved.iter().any(|(_, o)| o == p) || self.created.iter().any(|c| c == p) {
            return Ok(());
        }
        if p.exists() {
            let dest = self.dir.join(format!("{:03}-{}", self.saved.len(), p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()));
            fs::rename(p, &dest).map_err(|e| {
                RollbackError(format!(
                    "无法先把 {} 挪进回滚日志（{e}）——覆盖之后将无法还原，因此中止这次部署",
                    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                ))
            })?;
            self.saved.push((dest, p.to_path_buf()));
        } else {
            self.created.push(p.to_path_buf());
        }
        self.flush();
        Ok(())
    }

    /// 部署成功：后悔药作废。
    pub fn commit(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
        self.saved.clear();
        self.created.clear();
        self.drop_base_if_empty();
    }

    /// 还原到部署之前。返回 (是否完整还原, 说明)。
    pub fn rollback(&mut self) -> (bool, String) {
        if let Some(outcome) = &self.failed_outcome {
            return outcome.clone();
        }
        let (ok, detail) = Self::apply(&self.saved, &self.created);
        if ok {
            let _ = fs::remove_dir_all(&self.dir);
            self.saved.clear();
            self.created.clear();
            self.drop_base_if_empty();
        } else {
            self.failed_outcome = Some((ok, detail.clone()));
        }
        (ok, detail)
    }

    fn drop_base_if_empty(&self) {
        if let Some(parent) = self.dir.parent() {
            let _ = fs::remove_dir(parent);
        }
    }

    fn apply(saved: &[(PathBuf, PathBuf)], created: &[PathBuf]) -> (bool, String) {
        let mut problems: Vec<String> = Vec::new();

        for (dest, original) in saved {
            if !dest.exists() {
                problems.push(format!(
                    "{} 的备份副本已丢失，无法还原",
                    original.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                ));
            }
        }
        for p in created {
            if let Err(e) = fs::remove_file(p) {
                if p.exists() {
                    problems.push(format!(
                        "清除新建文件 {} 失败：{e}",
                        p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                    ));
                }
            }
        }
        for (dest, original) in saved.iter().rev() {
            if dest.exists() {
                if let Some(parent) = original.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                if let Err(e) = fs::rename(dest, original) {
                    problems.push(format!(
                        "还原 {} 失败：{e}",
                        original.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                    ));
                }
            }
        }
        for (_, original) in saved {
            if !original.exists() {
                problems.push(format!(
                    "{} 没有回到原位",
                    original.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                ));
            }
        }
        for p in created {
            if p.exists() {
                problems.push(format!(
                    "新建的 {} 没有被清掉",
                    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                ));
            }
        }

        if !problems.is_empty() {
            return (false, problems.join("；"));
        }
        if saved.is_empty() && created.is_empty() {
            return (true, "无需还原（部署在改动设备文件之前就失败了）".to_string());
        }
        let mut parts = Vec::new();
        if !saved.is_empty() {
            parts.push(format!("还原 {} 个被覆盖的文件", saved.len()));
        }
        if !created.is_empty() {
            parts.push(format!("清除 {} 个新建文件", created.len()));
        }
        (true, parts.join("，"))
    }

    fn flush(&self) {
        let saved: Vec<Value> = self
            .saved
            .iter()
            .map(|(d, o)| {
                serde_json::json!({"saved": d.to_string_lossy(), "original": o.to_string_lossy()})
            })
            .collect();
        let created: Vec<String> = self.created.iter().map(|p| p.to_string_lossy().to_string()).collect();
        let data = serde_json::json!({"saved": saved, "created": created});
        let _ = fs::create_dir_all(&self.dir);
        if let Ok(text) = serde_json::to_string_pretty(&data) {
            let _ = fs::write(self.dir.join(MANIFEST_NAME), text);
        }
    }

    fn read_manifest(&self) -> Option<serde_json::Map<String, serde_json::Value>> {
        let f = self.dir.join(MANIFEST_NAME);
        if !f.is_file() {
            return None;
        }
        let text = fs::read_to_string(f).ok()?;
        let data: serde_json::Value = serde_json::from_str(&text).ok()?;
        let obj = data.as_object()?;
        let has_saved = obj.get("saved").and_then(|s| s.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
        let has_created = obj.get("created").and_then(|s| s.as_array()).map(|a| !a.is_empty()).unwrap_or(false);
        if !has_saved && !has_created {
            return None;
        }
        Some(obj.clone())
    }
}

use serde_json::Value;

fn pairs(items: Option<&Value>) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    let arr = match items.and_then(|v| v.as_array()) {
        Some(a) => a,
        None => return out,
    };
    for item in arr {
        let obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let saved = obj.get("saved").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
        let original = obj.get("original").and_then(|s| s.as_str()).unwrap_or("").trim().to_string();
        if !saved.is_empty() && !original.is_empty() {
            out.push((PathBuf::from(saved), PathBuf::from(original)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::InstallJournal;
    use std::fs;
    use std::path::Path;

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
                "\n回滚日志验收失败 {} 项：\n{}",
                self.failures.len(),
                self.failures.join("\n")
            );
        }
    }

    fn read(path: &Path) -> Vec<u8> {
        fs::read(path).unwrap_or_default()
    }

    #[test]
    fn install_journal_semantics() {
        let mut r = Results::new();
        let base = std::env::temp_dir().join(format!(
            "edge-rollback-journal-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();

        // ---- B 覆盖还原：旧文件被保护起来，回滚后逐字节回到原位 ----
        {
            let work = base.join("a");
            fs::create_dir_all(&work).unwrap();
            let old = work.join("model.bin");
            fs::write(&old, b"OLD-VERSION").unwrap();

            let mut j = InstallJournal::new(&work.join(".rollback"), Some("task-a"));
            j.prepare().unwrap();
            j.capture(&old).unwrap();
            r.check(
                "B capture 之后旧文件已不在原位（说明它真的被保护起来了）",
                !old.exists(),
                "还在原位",
            );
            fs::write(&old, b"NEW-VERSION").unwrap();

            let (ok, note) = j.rollback();
            r.check("B 回滚报告成功", ok, note.clone());
            r.check(
                "B 旧文件回到原位，且**内容是原来那一份**",
                read(&old) == b"OLD-VERSION",
                format!("{:?}", read(&old)),
            );
            r.check("B 回滚说明写清了做了什么", note.contains("还原"), note.clone());
            r.check(
                "B 回滚完成后日志目录已清空",
                !work.join(".rollback").join("task-a").exists(),
                "日志目录还在",
            );
        }

        // ---- A 全新设备：目标文件原本不存在，回滚后必须不存在 ----
        {
            let work = base.join("a2");
            fs::create_dir_all(&work).unwrap();
            let mut j = InstallJournal::new(&work.join(".rollback"), Some("task-a2"));
            j.prepare().unwrap();
            let newfile = work.join("brand-new.bin");
            j.capture(&newfile).unwrap(); // 设备上原本没有它
            fs::write(&newfile, b"NEW").unwrap();
            let (ok, _note) = j.rollback();
            r.check(
                "A 原本不存在的文件，回滚后必须**不存在**",
                !newfile.exists(),
                "还在",
            );
            r.check("A 回滚报告成功", ok, "");
        }

        // ---- C 成功不回滚：commit 之后旧版本作废、日志目录清空 ----
        {
            let work = base.join("a3");
            fs::create_dir_all(&work).unwrap();
            let dst = work.join("model.bin");
            fs::write(&dst, b"OLD").unwrap();
            let mut j = InstallJournal::new(&work.join(".rollback"), Some("task-a3"));
            j.prepare().unwrap();
            j.capture(&dst).unwrap();
            fs::write(&dst, b"NEW").unwrap();
            j.commit();
            r.check(
                "C commit 之后保持新版内容不变",
                read(&dst) == b"NEW",
                format!("{:?}", read(&dst)),
            );
            r.check(
                "C commit 之后悔药已丢弃（旧版本不会再冒出来）",
                !work.join(".rollback").join("task-a3").exists(),
                "悔药还在",
            );
        }

        // ---- D 幂等：连滚两次不炸 ----
        {
            let work = base.join("d");
            fs::create_dir_all(&work).unwrap();
            let mut j = InstallJournal::new(&work.join(".rollback"), Some("task-d"));
            j.prepare().unwrap();
            let (ok1, _) = j.rollback();
            let (ok2, note2) = j.rollback();
            r.check("第一次回滚成功", ok1, "");
            r.check("第二次回滚不抛异常，且仍报成功", ok2, note2);
        }

        // ---- F 回滚失败必须如实报（本模块最重要的一条） ----
        {
            let work = base.join("f");
            fs::create_dir_all(&work).unwrap();
            let f = work.join("model.bin");
            fs::write(&f, b"OLD").unwrap();
            let mut j = InstallJournal::new(&work.join(".rollback"), Some("task-f"));
            j.prepare().unwrap();
            j.capture(&f).unwrap();
            fs::write(&f, b"NEW").unwrap();
            // 制造"还原不回去"：把救命的备份文件删掉
            let backup_dir = work.join(".rollback").join("task-f");
            for entry in fs::read_dir(&backup_dir).unwrap().flatten() {
                let p = entry.path();
                if p.file_name().map(|n| n.to_string_lossy().contains("model.bin")).unwrap_or(false) {
                    let _ = fs::remove_file(&p);
                }
            }
            let (ok, note) = j.rollback();
            r.check(
                "备份丢了 → 回滚必须返回 False，不能打勾",
                !ok,
                note.clone(),
            );
            r.check("失败说明里点出是哪个文件没回去", note.contains("model.bin"), note.clone());
            r.check(
                "回滚失败时**保留**日志目录，不把最后的机会也删掉",
                backup_dir.exists(),
                "日志目录被删了",
            );
        }

        // ---- H 崩溃可恢复：上一轮遗留的清单，下一轮必须先还原掉 ----
        {
            let work = base.join("h");
            fs::create_dir_all(&work).unwrap();
            let h = work.join("model.bin");
            fs::write(&h, b"OLD").unwrap();
            let mut j6 = InstallJournal::new(&work.join(".rollback"), Some("task-h"));
            j6.prepare().unwrap();
            j6.capture(&h).unwrap();
            fs::write(&h, b"NEW").unwrap();
            // 故意不 rollback 也不 commit —— 等价于进程在中间被杀掉
            r.check(
                "遗留现场的清单文件确实落盘了",
                work.join(".rollback").join("task-h").join("journal.json").is_file(),
                "journal.json 没落盘",
            );

            let mut j7 = InstallJournal::new(&work.join(".rollback"), Some("task-h"));
            let note = j7.prepare().unwrap();
            r.check("新一轮 prepare 先把上一轮遗留的还原了", note.contains("还原"), note.clone());
            r.check(
                "H 被挪走的旧文件回到了原位，内容是旧的",
                read(&h) == b"OLD",
                format!("{:?}", read(&h)),
            );
            let backup_dir = work.join(".rollback").join("task-h");
            let leftover = fs::read_dir(&backup_dir)
                .map(|it| {
                    it.flatten().any(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .contains("model.bin")
                    })
                })
                .unwrap_or(false);
            r.check("H 还原完才开新一轮：没有残留的旧备份", !leftover, "有残留备份");
        }

        // ---- J 路径安全：目录名不允许跑出 base ----
        {
            let outside = base.join("outside.bin");
            fs::write(&outside, b"SHOULD-NOT-BE-TOUCHED").unwrap();
            let jdir = base.join("j").join(".rollback");
            let mut j8 = InstallJournal::new(&jdir, Some("../../outside"));
            r.check(
                "带 ../ 的 task_id 没能逃出 base 目录",
                j8.dir.starts_with(&jdir),
                format!("{}", j8.dir.display()),
            );
            j8.prepare().unwrap();
            j8.rollback();
            r.check(
                "J 目录外面的文件毫发无损",
                read(&outside) == b"SHOULD-NOT-BE-TOUCHED",
                format!("{:?}", read(&outside)),
            );
        }

        // ---- E 拿不到后悔药就不动手：建不了回滚目录必须抛 RollbackError ----
        {
            let work = base.join("e");
            fs::create_dir_all(&work).unwrap();
            // 把回滚目录位置先占成一个普通文件 —— 建目录必然失败
            fs::write(work.join(".rollback"), "occupied").unwrap();
            let mut je = InstallJournal::new(&work.join(".rollback"), Some("task-e"));
            r.check(
                "建不了回滚目录时 prepare 必须抛 RollbackError",
                je.prepare().is_err(),
                "居然成功了",
            );
        }

        let _ = fs::remove_dir_all(&base);
        r.finish();
    }
}
