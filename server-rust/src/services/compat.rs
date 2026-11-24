//! 模型 ↔ 节点兼容性检查（需求 §5「部署前检查 SoC / QNN 兼容性」）。
//!
//! 铁律：拿不到 ≠ 不兼容。两边都拿到值且明确冲突才拦。
//! 与 Python services/compat.py 对齐（含 SoC 别名表）。

use serde_json::Value;

/// 模型侧参与兼容性判定的字段。
pub struct CompatModel {
    pub target_soc: String,
    pub qnn_version: String,
    pub framework: String,
    pub backends: Option<Vec<String>>,
}

/// 节点侧参与兼容性判定的字段。
pub struct CompatNode {
    pub soc: String,
    pub qnn_version: String,
    pub capabilities: Value,
}

// 已知 SoC 别名：同一颗芯片的各种写法归到同一个规范名。
const SOC_ALIASES: &[(&str, &[&str])] = &[
    ("sm8550", &["sm8550", "snapdragon8gen2", "sd8gen2", "8gen2"]),
    ("sm8650", &["sm8650", "snapdragon8gen3", "sd8gen3", "8gen3"]),
    ("sm8450", &["sm8450", "snapdragon8gen1", "sd8gen1", "8gen1"]),
    ("sm8475", &["sm8475", "snapdragon8plusgen1", "sd8plusgen1", "8plusgen1"]),
    ("sm8250", &["sm8250", "snapdragon870", "sd870"]),
    ("sm8350", &["sm8350", "snapdragon888", "sd888"]),
    ("sm7325", &["sm7325", "snapdragon778g", "sd778g"]),
    ("sm6375", &["sm6375", "snapdragon695", "sd695"]),
];

fn norm_soc(v: &str) -> String {
    v.to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

fn soc_canonical(v: &str) -> Option<&'static str> {
    let n = norm_soc(v);
    if n.is_empty() {
        return None;
    }
    for (canonical, aliases) in SOC_ALIASES {
        if aliases.contains(&n.as_str()) {
            return Some(canonical);
        }
    }
    None
}

fn split_soc_list(v: &str) -> Vec<String> {
    v.split(|c| matches!(c, ',' | ';' | '/' | '|'))
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect()
}

fn parse_version(v: &str) -> Option<Vec<i64>> {
    let mut out: Vec<i64> = Vec::new();
    let mut cur = String::new();
    for c in v.chars() {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            out.push(cur.parse::<i64>().ok()?);
            cur.clear();
        }
    }
    if !cur.is_empty() {
        out.push(cur.parse::<i64>().ok()?);
    }
    if out.is_empty() {
        None
    } else {
        Some(out[..4.min(out.len())].to_vec())
    }
}

fn cmp_versions(a: &[i64], b: &[i64]) -> std::cmp::Ordering {
    let n = a.len().max(b.len());
    let mut a2 = a.to_vec();
    a2.resize(n, 0);
    let mut b2 = b.to_vec();
    b2.resize(n, 0);
    a2.cmp(&b2)
}

struct Check {
    name: &'static str,
    verdict: &'static str,
    detail: String,
    want: String,
    got: String,
}

impl Check {
    fn json(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "verdict": self.verdict,
            "detail": self.detail,
            "want": self.want,
            "got": self.got,
        })
    }

    fn unknown(name: &'static str, detail: &str, want: &str, got: &str) -> Self {
        Self { name, verdict: "unknown", detail: detail.to_string(), want: want.to_string(), got: got.to_string() }
    }

    fn pass(name: &'static str, detail: &str, want: &str, got: &str) -> Self {
        Self { name, verdict: "pass", detail: detail.to_string(), want: want.to_string(), got: got.to_string() }
    }

    fn fail(name: &'static str, detail: &str, want: &str, got: &str) -> Self {
        Self { name, verdict: "fail", detail: detail.to_string(), want: want.to_string(), got: got.to_string() }
    }
}

fn check_soc(model: &CompatModel, node: &CompatNode) -> Check {
    let want = model.target_soc.trim().to_string();
    let got = node.soc.trim().to_string();
    if want.is_empty() {
        return Check::unknown("soc", "模型未登记目标 SoC", "", &got);
    }
    if got.is_empty() {
        return Check::unknown("soc", "设备未上报 SoC", &want, "");
    }

    let node_canonical = soc_canonical(&got);
    let tokens = split_soc_list(&want);
    for tok in &tokens {
        if norm_soc(tok) == norm_soc(&got) {
            return Check::pass("soc", "SoC 直接匹配", &want, &got);
        }
        let tok_canonical = soc_canonical(tok);
        if let (Some(tc), Some(nc)) = (tok_canonical, node_canonical) {
            if tc == nc {
                return Check::pass("soc", &format!("SoC 别名匹配（{} ≈ {}）", tok, got), &want, &got);
            }
        }
    }

    // 有值但认不出来（不在别名表里）→ 不判失败，如实标 unknown。
    if node_canonical.is_none() && tokens.iter().all(|t| soc_canonical(t).is_none()) {
        return Check::unknown("soc", "SoC 写法不在已知别名表内，无法判断", &want, &got);
    }

    Check::fail(
        "soc",
        &format!("设备 SoC 不在模型支持范围内（模型支持 {}，设备是 {}）", want, got),
        &want,
        &got,
    )
}

fn check_qnn(model: &CompatModel, node: &CompatNode) -> Check {
    let want = model.qnn_version.trim().to_string();
    let got = node.qnn_version.trim().to_string();
    if want.is_empty() {
        return Check::unknown("qnn_version", "模型未登记 QNN 版本要求", "", &got);
    }
    if got.is_empty() {
        return Check::unknown("qnn_version", "设备未上报 QNN 版本", &want, "");
    }
    let (need, have) = match (parse_version(&want), parse_version(&got)) {
        (Some(a), Some(b)) => (a, b),
        _ => return Check::unknown("qnn_version", "版本号格式无法解析", &want, &got),
    };
    if cmp_versions(&have, &need) != std::cmp::Ordering::Less {
        return Check::pass("qnn_version", "设备 QNN 版本满足要求", &want, &got);
    }
    Check::fail(
        "qnn_version",
        &format!("设备 QNN 版本过低（需要 ≥ {}，设备是 {}）", want, got),
        &want,
        &got,
    )
}

fn check_qnn_runtime(model: &CompatModel, node: &CompatNode) -> Check {
    if model.framework != "qnn" {
        return Check::unknown("qnn_runtime", "模型不使用 QNN 框架", "", "");
    }
    let caps = &node.capabilities;
    if !caps.is_object() {
        return Check::unknown("qnn_runtime", "设备能力表不可用", "", "");
    }
    let val = caps.get("qnn_runtime").and_then(|v| v.as_i64());
    match val {
        None | Some(-1) => Check::unknown("qnn_runtime", "设备未上报 QNN Runtime 能力", "", ""),
        Some(1) => Check::pass("qnn_runtime", "设备具备 QNN Runtime", "", ""),
        Some(v) => Check::fail("qnn_runtime", "模型需要 QNN 框架，但设备没有 QNN Runtime", "qnn_runtime=1", &v.to_string()),
    }
}

fn check_backends(model: &CompatModel, node: &CompatNode) -> Check {
    let want = match &model.backends {
        Some(w) if !w.is_empty() => w.clone(),
        _ => return Check::unknown("backends", "模型未声明 Backend", "", ""),
    };
    let have: Option<Vec<String>> = node
        .capabilities
        .get("backends")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect());
    let have = match have {
        Some(h) if !h.is_empty() => h,
        _ => {
            return Check::unknown(
                "backends",
                "设备未上报 Backend 支持列表",
                &want.join(","),
                "",
            )
        }
    };

    let lowered: Vec<String> = have.iter().map(|x| x.trim().to_lowercase()).collect();
    let hit: Vec<String> = want
        .iter()
        .filter(|x| lowered.contains(&x.trim().to_lowercase()))
        .cloned()
        .collect();
    if !hit.is_empty() {
        return Check::pass("backends", &format!("命中 Backend：{}", hit.join(", ")), &want.join(","), &have.join(","));
    }
    Check::fail(
        "backends",
        &format!(
            "无可用 Backend（模型需要 {}，设备支持 {}）",
            want.join(", "),
            have.join(", ")
        ),
        &want.join(","),
        &have.join(","),
    )
}

/// 跑完所有维度，返回整体结论（JSON：{ok, reason, checks}）。
pub fn check_compatibility(model: &CompatModel, node: &CompatNode) -> Value {
    let checks = vec![
        check_soc(model, node),
        check_qnn(model, node),
        check_qnn_runtime(model, node),
        check_backends(model, node),
    ];
    let failure = checks.iter().find(|c| c.verdict == "fail");
    let (ok, reason) = match failure {
        Some(f) => (false, f.detail.clone()),
        None => (true, String::new()),
    };
    serde_json::json!({
        "ok": ok,
        "reason": reason,
        "checks": checks.iter().map(|c| c.json()).collect::<Vec<_>>(),
    })
}
