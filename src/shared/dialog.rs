//! 启动失败 / 崩溃时的可见出口。
//!
//! 为什么需要：安装器和主控台都是 `windows_subsystem = "windows"` 的 GUI ——
//! 客户机上没有控制台可看，`main` 返回 `Err` 就等于静默退出。实测过：在一台
//! Win11 电脑上双击安装程序「什么都没发生」，而我们拿不到任何线索。
//!
//! 这里做两件事，都不依赖 GUI 框架：
//!
//! 1. `write_failure`：把原因追加进日志文件 —— 事后能查、能发给我们；
//! 2. `report_failure`：再弹一个系统原生提示框 —— 用户当场就知道出了什么事。
//!
//! 用 `user32!MessageBoxW`（项目已依赖 `windows-sys`），不引入任何新东西：
//! 出错的场合恰恰是最不该再依赖更多东西的场合。

use std::io::Write;
use std::path::{Path, PathBuf};

/// 把失败原因追加进日志文件，返回日志路径（写不进去时返回空串）。
pub fn write_failure(log: &Path, scope: &str, message: &str) -> String {
    let line = format!("[{}] {scope}: {message}\n", timestamp());
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    else {
        return String::new();
    };
    if file.write_all(line.as_bytes()).is_err() {
        return String::new();
    }
    log.display().to_string()
}

/// 写日志 + 弹原生提示框。返回日志路径（写不进去时为空串）。
pub fn report_failure(log: &Path, scope: &str, message: &str) -> String {
    let path = write_failure(log, scope, message);
    let text = if path.is_empty() {
        format!("{scope}\n\n{message}")
    } else {
        format!("{scope}\n\n{message}\n\n详细日志：{path}")
    };
    message_box("AICRM", &text, false);
    path
}

/// 弹一个原生提示框（成功场景）。用信息图标，而不是错误图标。
///
/// 只有**无界面安装**这条路用得上：那台机器的显卡只给了 OpenGL 1.1，窗口在创建
/// 阶段就失败，这个框是用户唯一能看到的结果 —— 不弹就等于"双击了没反应"。
///
/// 刻意只弹框、不写日志：成功时用户要的是"装好了"这句话，不是一串路径；
/// "这次走了无界面安装"这件事，在降级那一刻已经记进日志了。
pub fn report_success(scope: &str, message: &str) {
    message_box("AICRM", &format!("{scope}\n\n{message}"), true);
}

/// 崩溃兜底：panic 同样留痕并弹提示 —— GUI 里 panic 只会让窗口凭空消失。
pub fn install_panic_reporter(log: PathBuf, scope: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 提示框会一直等到用户点确定；panic 本来就是异常路径，挡在这里可以接受。
        report_failure(&log, scope, &format!("内部错误：{info}"));
        previous(info);
    }));
}
#[cfg(windows)]
fn message_box(title: &str, text: &str, information: bool) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    };
    let icon = if information {
        MB_ICONINFORMATION
    } else {
        MB_ICONERROR
    };
    let wide = |value: &str| -> Vec<u16> {
        std::os::windows::ffi::OsStrExt::encode_wide(std::ffi::OsStr::new(value))
            .chain(std::iter::once(0))
            .collect()
    };
    let text = wide(text);
    let caption = wide(title);
    // SAFETY: 两个缓冲都以 NUL 结尾、在本调用期间一直存活；MessageBoxW 只在返回前读它们。
    // 置顶 + 置前是刻意的：这个提示出现在"用户以为程序没启动"的时刻，
    // 落在别的窗口后面就等于没提示。
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            caption.as_ptr(),
            MB_OK | icon | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(not(windows))]
fn message_box(title: &str, text: &str, _information: bool) {
    eprintln!("{title}\n{text}");
}

/// `YYYY-MM-DD HH:MM:SSZ`（UTC）。只为一个日志时间戳引入时间库不值得。
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}Z",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// 把「距 1970-01-01 的天数」换成公历日期（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_match_known_epochs() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_000), (2024, 10, 4));
        // 取自真实签发的 JWT iat：2026-10-06 18:49:01 +08:00
        assert_eq!(civil_from_days(1_791_283_741 / 86_400), (2026, 10, 6));
    }

    #[test]
    fn failure_log_is_appended_with_the_reason() {
        let temp = tempfile::tempdir().unwrap();
        let log = temp.path().join("nested").join("install.err.log");
        let path = write_failure(&log, "安装程序启动失败", "无法创建窗口");
        assert_eq!(path, log.display().to_string());
        let first = std::fs::read_to_string(&log).unwrap();
        assert!(first.contains("安装程序启动失败: 无法创建窗口"), "{first}");
        write_failure(&log, "安装程序启动失败", "第二次");
        let second = std::fs::read_to_string(&log).unwrap();
        assert!(second.starts_with(&first), "日志应当是追加而不是覆盖");
        assert!(second.contains("第二次"), "{second}");
    }
}
