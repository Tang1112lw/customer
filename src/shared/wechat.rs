//! 找本机微信程序（`Weixin.exe` / `WeChat.exe`）的完整路径。
//!
//! 为什么要客户端自己找：抓密钥的第三方工具对安装位置很挑，找不到就把它自己的
//! 原话甩给客户 ——「无法定位微信程序（无法自动找到 Weixin.exe；可使用 --weixin-exe
//! 指定）」。客户不可能知道该填什么路径。客户端替它找好、明确传 `--weixin-exe`，
//! 是这里唯一的目的。
//!
//! 顺序：**注册表**（装在 D 盘等非默认目录时只有这里查得到）→ **常见安装目录**
//! （4.x 的 `Weixin`、3.x 的 `WeChat`）。都找不到返回 `None`，由调用方给一句
//! 人能照做的提示，而不是把工具的原话抛出去。

use std::path::PathBuf;

/// 微信 4.x（Weixin）与 3.x（WeChat）的卸载登记项：`DisplayIcon` / `InstallLocation`
/// 两个值里通常有一个指向真实安装目录。
#[cfg(windows)]
const REGISTRY_KEYS: [&str; 6] = [
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\Weixin",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\Weixin",
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\WeChat",
    r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\WeChat",
    r"SOFTWARE\Tencent\Weixin",
    r"SOFTWARE\Tencent\WeChat",
];

/// 本机微信可执行文件。找不到返回 `None`。
pub fn find_weixin_exe() -> Option<PathBuf> {
    let mut candidates = registry_candidates();
    candidates.extend(common_directories());
    candidates.into_iter().find(|path| path.is_file())
}

/// 注册表里的候选路径（按登记项顺序）。
fn registry_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    #[cfg(windows)]
    {
        for subkey in REGISTRY_KEYS {
            for root in [win::HKEY_LOCAL_MACHINE, win::HKEY_CURRENT_USER] {
                for value in ["DisplayIcon", "InstallLocation", "InstallPath", "Path"] {
                    let Some(raw) = win::read_string(root, subkey, value) else {
                        continue;
                    };
                    candidates.extend(paths_from_registry_value(&raw));
                }
            }
        }
    }
    candidates
}

/// 把一个注册表值变成"可能是 exe 的路径"。
///
/// `DisplayIcon` 形如 `"C:\...\Weixin.exe",0`；`InstallLocation` 形如 `D:\Weixin\`。
fn paths_from_registry_value(raw: &str) -> Vec<PathBuf> {
    let trimmed = raw.trim().trim_matches('"');
    // 去掉 DisplayIcon 末尾的图标序号：`,0` / `,-12`
    let without_index = match trimmed.rsplit_once(',') {
        Some((head, tail)) if tail.trim().parse::<i32>().is_ok() => head,
        _ => trimmed,
    };
    let path = PathBuf::from(without_index.trim().trim_matches('"'));
    if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe")) {
        return vec![path];
    }
    // 目录：把两种可能的程序名都拼上
    vec![path.join("Weixin.exe"), path.join("WeChat.exe")]
}

/// 常见安装目录（默认路径装的就走这里，不必依赖注册表）。
fn common_directories() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for name in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(value) = std::env::var_os(name) {
            roots.push(PathBuf::from(value));
        }
    }
    let mut candidates = Vec::new();
    for root in roots {
        candidates.push(root.join("Tencent").join("Weixin").join("Weixin.exe"));
        candidates.push(root.join("Tencent").join("WeChat").join("WeChat.exe"));
    }
    candidates
}

#[cfg(windows)]
mod win {
    use std::os::windows::ffi::OsStrExt;
    pub use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    use windows_sys::Win32::System::Registry::{HKEY, RegGetValueW, RRF_RT_REG_SZ};

    /// 读一个 REG_SZ 值。键/值不存在一律返回 None（注册表里查不到是常态，不是错误）。
    pub fn read_string(root: HKEY, subkey: &str, value: &str) -> Option<String> {
        let subkey = wide(subkey);
        let value = wide(value);
        let mut buffer = [0u16; 1024];
        let mut size = std::mem::size_of_val(&buffer) as u32;
        // SAFETY: 两个字符串都以 NUL 结尾且在整个调用期间存活；buffer/size 描述同一块内存。
        let status = unsafe {
            RegGetValueW(
                root,
                subkey.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buffer.as_mut_ptr().cast(),
                &mut size,
            )
        };
        if status != 0 {
            return None;
        }
        // size 是字节数，含结尾的 NUL
        let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
        let text = String::from_utf16_lossy(&buffer[..length]);
        (!text.trim().is_empty()).then_some(text)
    }

    fn wide(value: &str) -> Vec<u16> {
        std::ffi::OsStr::new(value)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_icon_is_trimmed_to_the_executable() {
        let paths = paths_from_registry_value(r#""C:\Program Files\Tencent\Weixin\Weixin.exe",0"#);
        assert_eq!(paths, vec![PathBuf::from(r"C:\Program Files\Tencent\Weixin\Weixin.exe")]);
        // 没有图标序号、也没有引号的写法
        let paths = paths_from_registry_value(r"D:\Weixin\Weixin.exe");
        assert_eq!(paths, vec![PathBuf::from(r"D:\Weixin\Weixin.exe")]);
    }

    #[test]
    fn install_location_becomes_both_program_names() {
        let paths = paths_from_registry_value(r"D:\Program Files\Tencent\Weixin\");
        assert!(paths.iter().any(|path| path.ends_with("Weixin.exe")), "{paths:?}");
        assert!(paths.iter().any(|path| path.ends_with("WeChat.exe")), "{paths:?}");
    }

    /// 本机自检：能定位到时，返回的路径必须真的存在（在开发机上跑一次就知道找得对不对）。
    #[test]
    fn located_executable_must_exist() {
        let found = find_weixin_exe();
        eprintln!("本机微信：{found:?}");
        if let Some(path) = found {
            assert!(path.is_file(), "{} 应当存在", path.display());
        }
    }
}
