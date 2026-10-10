//! 安装目录与文件位置的**唯一约定**。
//!
//! 安装目录里住着什么、叫什么名字，以前在 console.rs、store.rs、update/main.rs、
//! install.ps1 里各写了一份 —— 改一处漏一处，README 里「设备身份有两处必须一致」
//! 那个坑就是这种散落逼出来的。Rust 侧的所有路径现在统一从这里取。
//!
//! `install.ps1` 仍按同一套约定落盘（它无法引入 Rust 模块），改动这里时
//! 必须同步核对脚本里的对应位置。

use std::path::PathBuf;

/// 安装目录：`%LOCALAPPDATA%\AICRM-Windows-API`。
pub fn install_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("AICRM-Windows-API")
}

/// 本地 API 的 Bearer 令牌。安装时生成，登录后换成云端会话令牌。
pub fn api_token() -> PathBuf {
    install_dir().join("api.token")
}

/// 刷新令牌。access token 只有 2 小时，服务靠它自动续期。
pub fn api_refresh() -> PathBuf {
    install_dir().join("api.refresh")
}

/// 云端基地址。隧道地址变了只改这个文件再重启服务，不用重装。
pub fn cloud_url() -> PathBuf {
    install_dir().join("cloud.url")
}

/// 设备标识。云端用它单独撤销某一台设备。
pub fn device_id() -> PathBuf {
    install_dir().join("device.id")
}

/// 设备令牌（配对时由云端确认，本机只存明文一份，权限已收紧）。
pub fn device_token() -> PathBuf {
    install_dir().join("device.token")
}

/// 登录账号名（登录成功后写入，主界面右上角读取）。
pub fn account_name() -> PathBuf {
    install_dir().join("account.name")
}
