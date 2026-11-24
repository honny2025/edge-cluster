//! 小工具：token / ID 生成。

use base64::Engine;
use rand::RngCore;

/// URL-safe 随机 token（对应 Python `secrets.token_urlsafe(32)`）。
pub fn new_urlsafe_token() -> String {
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// UUID hex（32 字符，无连字符）。用于 model_id / service_id / task_id / transfer_id 等。
pub fn new_uuid_hex() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// 指令 id（对应 Python `uuid.uuid4().hex[:12]` = 12 位十六进制）。
pub fn new_cmd_id() -> String {
    let mut buf = [0u8; 6];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// 短 id（12 位十六进制，用于下载 token 等一次性凭据）。与 new_cmd_id 同源。
#[allow(dead_code)] // 与 Python 版对齐
pub fn new_short_hex() -> String {
    new_cmd_id()
}
