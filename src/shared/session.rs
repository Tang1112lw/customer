//! 登录令牌的本地判定：不联网也能回答"现在算不算登录着"。
//!
//! 只看两件事：像不像云端会话令牌（三段式 JWT），以及**有没有过期**。
//!
//! 为什么必须判过期：access token 只有 2 小时，续期也可能失败（刷新令牌过期、长期
//! 断网、账号被禁用）。这时本地留着一个"看起来像令牌"的死凭据 —— 只判前缀的话，
//! 界面写着「已登录」、命令通道一直 401，用户完全看不出该重新登录（实测踩过：
//! 装完机器上还留着两天前的令牌，界面照旧显示某个账号已登录）。
//!
//! 不做签名校验：本地没有也不需要 JWT_SECRET；验签是云端的职责。这里只回答
//! "要不要引导用户重新登录"。

/// 令牌看起来是不是一个**没过期**的云端会话。
pub fn token_is_live(token: &str) -> bool {
    let token = token.trim();
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 || !token.starts_with("eyJ") {
        return false;
    }
    let Some(expiry) = expiry_seconds(parts[1]) else {
        return false;
    };
    let Ok(now) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    // 留 1 分钟余量：刚好在临界点上判"还活着"，免得刚登录就被要求重新登录。
    (now.as_secs() as i64) + 60 < expiry
}

/// 从 JWT 载荷里取 `exp`，手工解析（不为一个字段引入 base64 依赖）。
fn expiry_seconds(payload: &str) -> Option<i64> {
    let raw = decode_base64url(payload)?;
    let text = String::from_utf8(raw).ok()?;
    let key = "\"exp\"";
    let start = text.find(key)? + key.len();
    let rest = text[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse().ok()
}

/// base64url（无填充）解码。JWT 只用这套字母表，不需要处理 `+` `/` `=`。
fn decode_base64url(value: &str) -> Option<Vec<u8>> {
    fn index(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some((byte - b'A') as u32),
            b'a'..=b'z' => Some((byte - b'a') as u32 + 26),
            b'0'..=b'9' => Some((byte - b'0') as u32 + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }

    let mut out = Vec::with_capacity(value.len() / 4 * 3);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in value.bytes() {
        if byte == b'=' {
            break;
        }
        buffer = (buffer << 6) | index(byte)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用真实的 JWT 结构造一个载荷：{"user_id":"...","exp":<秒>}
    fn token_with_expiry(expiry: i64) -> String {
        format!("eyJhbGciOiJIUzI1NiJ9.{}.signature", encode_base64url(&format!(
            "{{\"user_id\":\"c28c613d\",\"exp\":{expiry}}}"
        )))
    }

    fn encode_base64url(text: &str) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in text.as_bytes().chunks(3) {
            let mut buffer = 0u32;
            for (index, byte) in chunk.iter().enumerate() {
                buffer |= (*byte as u32) << (16 - index * 8);
            }
            for index in 0..chunk.len() + 1 {
                out.push(ALPHABET[((buffer >> (18 - index * 6)) & 0x3f) as usize] as char);
            }
        }
        out
    }

    #[test]
    fn expired_token_is_not_a_login() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!(token_is_live(&token_with_expiry(now + 3600)));
        assert!(!token_is_live(&token_with_expiry(now - 1)));
    }

    #[test]
    fn local_random_token_is_not_a_login() {
        assert!(!token_is_live(""));
        assert!(!token_is_live("9f3c1a2b4c5d6e7f8a9b0c1d2e3f4a5b"));
        assert!(!token_is_live("eyJhbGciOiJIUzI1NiJ9.bm90anNvbg.signature"));
    }
}
