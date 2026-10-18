//! 极小的 Base64 标准字母表编码（RFC 4648 §4，带 `=` 填充）。
//!
//! 用途只有一个：把聊天归档里的语音媒体文件塞进 JSON 上传包 —— `.silk` 是二进制，
//! JSON 里放不下，转成 Base64 才能随 `days[].media[].content_base64` 一起走。
//!
//! 为什么不引第三方 crate：与 `sha256.rs` 同理由 —— 客户端的依赖表越长，部署面越大；
//! 而这台机器拉新 crate 还要过代理。编码本身只有二十来行，用 RFC 4648 §10 的
//! 标准测试向量兜底（见文件末尾的测试）。
//!
//! 只做编码不做解码：归档是单向的（客户端 → 云端），解码是云端 Go 那边的事。

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// 计算 Base64 标准编码。空输入返回空串。
pub fn encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // 每 3 字节凑成 24 位，再切成 4 个 6 位组；最后一组不足 3 字节时低位补零，
        // 补出来的位置用 `=` 占位 —— 所以只有最后一行会出现填充，中间行不会。
        let group = match chunk {
            [first] => (*first as u32) << 16,
            [first, second] => ((*first as u32) << 16) | ((*second as u32) << 8),
            [first, second, third] => {
                ((*first as u32) << 16) | ((*second as u32) << 8) | *third as u32
            }
            // chunks(3) 不可能给出别的形状；写出来只为让 match 完备，不必真的跑到
            _ => unreachable!("chunks(3) 每组最多 3 字节"),
        };
        out.push(ALPHABET[(group >> 18) as usize] as char);
        out.push(ALPHABET[((group >> 12) & 0x3f) as usize] as char);
        out.push(match chunk.len() {
            1 => '=',
            _ => ALPHABET[((group >> 6) & 0x3f) as usize] as char,
        });
        out.push(match chunk.len() {
            3 => ALPHABET[(group & 0x3f) as usize] as char,
            _ => '=',
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::encode;

    /// RFC 4648 §10 的标准测试向量。逐字符比对，不靠"解码回来一样"自证 ——
    /// 客户端只编码，没有配对的解码器可以互校。
    #[test]
    fn matches_rfc4648_vectors() {
        assert_eq!(encode(b""), "");
        assert_eq!(encode(b"f"), "Zg==");
        assert_eq!(encode(b"fo"), "Zm8=");
        assert_eq!(encode(b"foo"), "Zm9v");
        assert_eq!(encode(b"foob"), "Zm9vYg==");
        assert_eq!(encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode(b"foobar"), "Zm9vYmFy");
    }

    /// RFC 4648 §10 的二进制向量：覆盖高位字节与三种尾长，顺带证明 `+` `/` 会出现。
    #[test]
    fn matches_rfc4648_binary_vectors() {
        assert_eq!(encode(&[0x14, 0xfb, 0x9c, 0x03]), "FPucAw==");
        assert_eq!(encode(&[0x14, 0xfb, 0x9c, 0x03, 0xd9]), "FPucA9k=");
        assert_eq!(encode(&[0x14, 0xfb, 0x9c, 0x03, 0xd9, 0x7e]), "FPucA9l+");
    }

    /// 跨块边界：多组的中间行不允许出现 `=`，否则就是分组切错了。
    #[test]
    fn only_the_last_group_gets_padding() {
        let encoded = encode(&[b'a'; 9]); // 正好 3 个完整组，一个字符都不该填充
        assert_eq!(encoded, "YWFhYWFhYWFh");
        assert_eq!(encoded.len(), 12);
        assert!(!encoded.contains('='));
    }

    /// 语音文件是二进制，必须连 `0x00` 与高位字节一起正确编码。
    #[test]
    fn encodes_silent_binary_payload() {
        assert_eq!(encode(&[0x00, 0x00, 0x00]), "AAAA");
        assert_eq!(encode(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(encode(&[0x02, 0x53, 0x49, 0x4c, 0x4b]), "AlNJTEs=");
    }
}
