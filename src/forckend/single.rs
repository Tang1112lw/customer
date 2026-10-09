//! 界面级单实例：同一时间只允许一个 AICRM 界面窗口。
//!
//! 主界面与安装器**共用同一个互斥体**，所以两者不会同时出现 —— 用户看到的
//! 「客户端和安装器两个窗口并排」正是这里要消灭的情况。重复启动时把已有窗口
//! 叫到前台，而不是再开一个（微信、QQ 都是这个行为）。
//!
//! 为什么用命名互斥体而不是数进程：
//!
//! - 数进程要拉起 `tasklist`，慢、还会被同名子进程误判；
//! - 互斥体由内核持有，进程退出（含崩溃）系统自动释放，不会留下"幽灵锁"
//!   挡住下次启动；
//! - 判据是"有没有人持有"，不是"有几个同名进程"，所以主界面和安装器能互认。

use std::sync::OnceLock;

use windows_sys::Win32::{
    Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS},
    System::Threading::CreateMutexW,
    UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE},
};

/// 互斥体名。主界面与安装器共用同一个名字。
const MUTEX_NAME: &str = r"Local\AICRM.WinClient";

/// 已知界面窗口标题，按顺序试着把已有实例叫到前台。
const TITLES: [&str; 2] = ["AICRM", "AICRM 安装"];

/// 持有的句柄。**必须活到进程结束**，否则互斥提前失效。
static HANDLE: OnceLock<isize> = OnceLock::new();

/// 抢占单实例名额。`true` = 本进程是第一个，可以继续启动界面。
pub fn acquire() -> bool {
    let name: Vec<u16> = MUTEX_NAME.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 传入以 NUL 结尾的宽字符串；第二个参数 0 表示不请求初始所有权。
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        // 建不出来就不拦用户 —— 宁可多开一个窗口，也别让程序直接起不来。
        return true;
    }
    // GetLastError 必须在 CreateMutexW 之后立刻读，中间的 API 调用会覆盖它。
    let taken = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if taken {
        // 抢不到就别握着句柄，立刻放掉。
        unsafe { CloseHandle(handle) };
        return false;
    }
    let _ = HANDLE.set(handle as isize);
    true
}

/// 主动放掉名额。
///
/// 安装器装完要把主界面拉起来时**必须先调这个** —— 否则自己还握着互斥体，
/// 主界面一启动就被挡掉，表现就是"装完了但客户端弹不出来"。
// 只有安装器需要主动放锁（它装完要把主界面拉起来）；主界面自己靠进程退出
// 由内核回收即可，所以在它这个 bin 里本函数用不到。
#[allow(dead_code)]
pub fn release() {
    if let Some(value) = HANDLE.get() {
        // SAFETY: 句柄是本模块自己从 CreateMutexW 拿到的。
        unsafe { CloseHandle(*value as windows_sys::Win32::Foundation::HANDLE) };
    }
}

/// 把已存在的界面窗口切到前台。找到返回 `true`。
///
/// 窗口可能停在托盘里（被移到屏幕外），所以先 `SW_RESTORE` 拉回来。
pub fn activate_existing() -> bool {
    for title in TITLES {
        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: 以 NUL 结尾的宽字符串；窗口类传空表示只按标题查找。
        let found = unsafe { FindWindowW(std::ptr::null(), wide.as_ptr()) };
        if found.is_null() {
            continue;
        }
        // SAFETY: found 是刚查到的有效窗口句柄。
        unsafe {
            ShowWindow(found, SW_RESTORE);
            SetForegroundWindow(found);
        }
        return true;
    }
    false
}