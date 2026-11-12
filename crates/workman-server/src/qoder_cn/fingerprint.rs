//! 设备指纹派生（COSY 头里的 machine 系列字段）。
//!
//! 对照 `pi-desktop/lib/qoder-cn/fingerprint.ts`。上游风控按「一台设备」聚类：
//! 同一账号长期来自同一台机器是正常形态，不同账号共用一个指纹会被关联。所以这些
//! 值一律**按账号 UID 单向派生**，而不是随机生成（随机值会让同一账号每请求换设备，
//! 同样触发风控）。
//!
//! 派生规则与 qoder2api-hub 国内版一致：
//!
//! | 字段 | 规则 |
//! |---|---|
//! | `machineId` | `md5("<salt>:<uid>")` 改造成 UUIDv4 形态（版本位 4、变体位 a） |
//! | `machineType` | `machineId` 去横线后取前 18 位 |
//! | `machineToken` | `sha512("machinetoken:<uid>")` 的 base64url 前 43 位 |
//! | `hostname` | `"DESKTOP-" + md5("hostname:<uid>")` 前 7 位大写 |

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use md5::{Digest as Md5Digest, Md5};
use sha2::Sha512;

/// `cosy-machineos`：官方按运行系统上报 `{arch}_{platform}`。
///
/// 这里固定官方默认的 `x86_64_linux`（与 qoder2api-hub 的默认值一致）：账号一旦
/// 按某个形态建档，换形态等于换设备，没必要跟随宿主机漂移。
pub const MACHINE_OS: &str = "x86_64_linux";

fn md5_hex(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// 稳定的 UUIDv4 形态派生值（同一 uid + salt 永远相同）。
pub fn derive_device_id(uid: &str, salt: &str) -> String {
    let subject = if uid.is_empty() { "anonymous" } else { uid };
    let hash = md5_hex(&format!("{salt}:{subject}"));
    // 伪造 UUIDv4 的版本位（第 13 位）与变体位（第 17 位），与官方 machineId 同形。
    let uuidish = format!("{}4{}a{}", &hash[..12], &hash[13..16], &hash[17..]);
    format!(
        "{}-{}-{}-{}-{}",
        &uuidish[0..8],
        &uuidish[8..12],
        &uuidish[12..16],
        &uuidish[16..20],
        &uuidish[20..]
    )
}

pub fn derive_machine_type(uid: &str) -> String {
    derive_device_id(uid, "machinetype")
        .replace('-', "")
        .chars()
        .take(18)
        .collect()
}

pub fn derive_machine_token(uid: &str) -> String {
    let mut hasher = Sha512::new();
    hasher.update(format!("machinetoken:{uid}").as_bytes());
    let digest = hasher.finalize();
    URL_SAFE_NO_PAD.encode(digest).chars().take(43).collect()
}

pub fn derive_machine_hostname(uid: &str) -> String {
    let subject = if uid.is_empty() { "anonymous" } else { uid };
    let hash = md5_hex(&format!("hostname:{subject}"));
    format!("DESKTOP-{}", hash[..7].to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 期望值由 `pi-desktop/lib/qoder-cn/fingerprint.ts` 生成（uid = `u-123`）。
    #[test]
    fn matches_the_pi_derivation() {
        let uid = "u-123";
        assert_eq!(
            derive_device_id(uid, "machine"),
            "98b53497-f10c-4475-a88a-9054ddd676f0"
        );
        assert_eq!(derive_machine_type(uid), "e356ad30e72c47faa4");
        assert_eq!(
            derive_machine_token(uid),
            "PPYDUrtP_l6EVckIdx7U_Rjuv8I8mn4sV5PWCSsJH0r"
        );
        assert_eq!(derive_machine_hostname(uid), "DESKTOP-A35307F");
        // 空 uid 走 "anonymous"，与 pi 一致。
        assert_eq!(
            derive_device_id("", "machine"),
            "fc1f3f60-3c8b-4dc0-a839-42ea0169ab1e"
        );
        assert_eq!(derive_machine_hostname(""), "DESKTOP-D72F009");
    }
}
