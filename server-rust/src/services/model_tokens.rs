//! 模型下载令牌（需求 §5 下发 / §8 安全）。只放内存，平台重启后旧令牌失效。
//! 与 Python services/model_tokens.py 对齐。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::config;
use crate::security;

struct Entry {
    model_id: String,
    #[allow(dead_code)] // 与 Python 版令牌结构对齐，node_id 预留按节点限定令牌
    node_id: String,
    expires_at: f64,
}

pub struct DownloadTokens {
    tokens: Mutex<HashMap<String, Entry>>,
}

fn now_epoch() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

impl DownloadTokens {
    pub fn new() -> Self {
        Self { tokens: Mutex::new(HashMap::new()) }
    }

    fn gc(&self) {
        let now = now_epoch();
        let mut tokens = self.tokens.lock().unwrap();
        tokens.retain(|_, e| e.expires_at >= now);
    }

    /// 给一次下发签发令牌。同一 (模型, 节点) 重复下发签发新令牌，不复用。
    pub fn issue(&self, model_id: &str, node_id: &str) -> String {
        self.gc();
        let token = crate::util::new_urlsafe_token();
        let ttl = config::get().files.transfer_ttl_minutes.max(1) as f64 * 60.0;
        let entry = Entry {
            model_id: model_id.to_string(),
            node_id: node_id.to_string(),
            expires_at: now_epoch() + ttl,
        };
        self.tokens.lock().unwrap().insert(token.clone(), entry);
        token
    }

    /// 校验令牌是否可用于下载该模型。常数时间比较。
    pub fn authorize(&self, model_id: &str, token: &str) -> bool {
        if model_id.is_empty() || token.is_empty() {
            return false;
        }
        self.gc();
        let mut tokens = self.tokens.lock().unwrap();
        match tokens.get(token) {
            None => false,
            Some(entry) => {
                if entry.expires_at < now_epoch() {
                    tokens.remove(token);
                    return false;
                }
                security::constant_time_eq(&entry.model_id, model_id)
            }
        }
    }

    /// 模型被删除时吊销它的全部下载令牌。返回吊销数量。
    pub fn revoke_model(&self, model_id: &str) -> usize {
        let mut tokens = self.tokens.lock().unwrap();
        let before = tokens.len();
        tokens.retain(|_, e| e.model_id != model_id);
        before - tokens.len()
    }
}

impl Default for DownloadTokens {
    fn default() -> Self {
        Self::new()
    }
}
