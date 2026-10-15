//! 敏感字段的本地加密（AES-256-GCM）。
//!
//! 密钥由外壳提供：
//! - Android：Kotlin 生成随机密钥，用 Android Keystore 包裹后存 SharedPreferences，
//!   启动时解包并通过 JNI 传给引擎；
//! - 桌面（开发）：环境变量 `AI_NOVEL_SECRET_HEX`（64 位 hex）。
//! 未提供密钥时退化为明文存储（本地开发方便，日志会提示）。
//!
//! 密文格式：`enc:v1:<base64(nonce(12B) || ciphertext)>`；非该前缀的值按明文读取，
//! 便于旧数据平滑迁移。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine as _;

pub const ENC_PREFIX: &str = "enc:v1:";

#[derive(Clone)]
pub struct SecretBox {
    cipher: Aes256Gcm,
}

impl SecretBox {
    pub fn from_hex(hex: &str) -> Option<Self> {
        let bytes = decode_hex(hex.trim())?;
        if bytes.len() != 32 {
            return None;
        }
        let key = Key::<Aes256Gcm>::from_slice(&bytes);
        Some(Self {
            cipher: Aes256Gcm::new(key),
        })
    }

    /// 加密（空串原样返回空串；随机数失败时退化为明文，避免阻断保存）
    pub fn encrypt(&self, plain: &str) -> String {
        if plain.is_empty() {
            return String::new();
        }
        let mut nonce_bytes = [0u8; 12];
        if getrandom::getrandom(&mut nonce_bytes).is_err() {
            return plain.to_string();
        }
        let nonce = Nonce::from_slice(&nonce_bytes);
        match self.cipher.encrypt(nonce, plain.as_bytes()) {
            Ok(ct) => {
                let mut buf = Vec::with_capacity(12 + ct.len());
                buf.extend_from_slice(&nonce_bytes);
                buf.extend_from_slice(&ct);
                format!(
                    "{ENC_PREFIX}{}",
                    base64::engine::general_purpose::STANDARD.encode(buf)
                )
            }
            Err(_) => plain.to_string(),
        }
    }

    /// 解密；非密文前缀原样返回；密文损坏或密钥不匹配返回 None
    pub fn decrypt(&self, stored: &str) -> Option<String> {
        if stored.is_empty() {
            return Some(String::new());
        }
        let Some(b64) = stored.strip_prefix(ENC_PREFIX) else {
            return Some(stored.to_string());
        };
        let buf = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        if buf.len() < 13 {
            return None;
        }
        let (nonce_bytes, ct) = buf.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        let pt = self.cipher.decrypt(nonce, ct).ok()?;
        String::from_utf8(pt).ok()
    }
}

pub fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    let hex = hex.trim();
    if hex.len() % 2 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        out.push(val(bytes[i])? << 4 | val(bytes[i + 1])?);
        i += 2;
    }
    Some(out)
}

pub fn encode_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_a() -> SecretBox {
        SecretBox::from_hex(&"ab".repeat(32)).unwrap()
    }

    fn key_b() -> SecretBox {
        SecretBox::from_hex(&"cd".repeat(32)).unwrap()
    }

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let sb = key_a();
        let ct = sb.encrypt("sk-1234567890");
        assert!(ct.starts_with(ENC_PREFIX), "ciphertext should be prefixed: {ct}");
        assert_ne!(ct, "sk-1234567890");
        assert_eq!(sb.decrypt(&ct).as_deref(), Some("sk-1234567890"));
    }

    #[test]
    fn encrypt_is_randomized_but_decrypts() {
        let sb = key_a();
        let a = sb.encrypt("same-input");
        let b = sb.encrypt("same-input");
        assert_ne!(a, b, "nonce 必须随机，两次密文不应相同");
        assert_eq!(sb.decrypt(&a).as_deref(), Some("same-input"));
        assert_eq!(sb.decrypt(&b).as_deref(), Some("same-input"));
    }

    #[test]
    fn plaintext_passthrough_and_empty() {
        let sb = key_a();
        assert_eq!(sb.decrypt("").as_deref(), Some(""));
        assert_eq!(sb.decrypt("not-encrypted").as_deref(), Some("not-encrypted"));
        assert_eq!(sb.encrypt(""), "");
    }

    #[test]
    fn wrong_key_fails() {
        let ct = key_a().encrypt("secret");
        assert_eq!(key_b().decrypt(&ct), None, "密钥不匹配必须失败而不是返回垃圾");
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let sb = key_a();
        let ct = sb.encrypt("secret");
        let mut raw = base64::engine::general_purpose::STANDARD
            .decode(ct.strip_prefix(ENC_PREFIX).unwrap())
            .unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01; // 篡改一个字节
        let tampered = format!(
            "{ENC_PREFIX}{}",
            base64::engine::general_purpose::STANDARD.encode(raw)
        );
        assert_eq!(sb.decrypt(&tampered), None, "GCM 必须检测到篡改");
    }

    #[test]
    fn hex_helpers() {
        let bytes: Vec<u8> = (0u8..8).collect();
        let hex = encode_hex(&bytes);
        assert_eq!(hex, "0001020304050607");
        assert_eq!(decode_hex(&hex).unwrap(), bytes);
        assert_eq!(decode_hex("abc"), None, "奇数长度必须拒绝");
        assert_eq!(decode_hex("zz"), None, "非 hex 字符必须拒绝");
        assert_eq!(SecretBox::from_hex("ab").is_none(), true, "长度不足必须拒绝");
    }
}
