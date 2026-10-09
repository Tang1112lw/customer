//! 桌面与开始菜单入口：程序自己建、自己清。
//!
//! 用 COM 的 `IShellLink` 建快捷方式（Windows 官方接口，图标与目标一定正确），
//! **不依赖 PowerShell / WScript.Shell** —— 客户机上脚本宿主常被执行策略或安全
//! 软件拦掉，而自更新那条路必须能建入口，所以两处共用这一份实现。
//!
//! 另外，装过几次的机器桌面上会留下旧版本入口与过期安装程序，客户点上去只看到
//! 「找不到文件」。这些也由程序自己收拾干净 —— 客户不该被要求手动删。

use std::path::{Path, PathBuf};
use std::process::Command;

/// 客户端唯一的入口名。不带版本号 —— 客户不需要知道版本。
pub const SHORTCUT_NAME: &str = "AICRM.lnk";

/// 更新完的收尾：清旧入口 → 重建规范入口 → 归档过期安装包 → 刷图标缓存。
pub fn refresh(install_dir: &Path, log_file: &Path) -> Result<(), String> {
    let target = install_dir.join("aicrm-console.exe");
    if !target.is_file() {
        return Err(format!("找不到主程序：{}", target.display()));
    }
    let roots = shortcut_roots();
    let removed = cleanup_links(&roots, install_dir);
    let created = write_links(&roots, &target, install_dir);
    let archived = archive_stale_packages(&roots, install_dir, log_file);
    log(
        log_file,
        &format!("入口：清理 {removed} 个、重建 {created} 个；归档过期安装包 {archived} 个"),
    );
    refresh_icon_cache();
    if created == 0 {
        return Err("没有可写的快捷方式位置".to_owned());
    }
    Ok(())
}

// ---------------------------------------------------------------- 目录

/// 快捷方式所在目录。只碰这几个已知位置，不做全盘搜索。
fn shortcut_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        roots.push(home.join("Desktop"));
        roots.push(home.join("Downloads"));
    }
    if let Some(public) = std::env::var_os("PUBLIC").map(PathBuf::from) {
        roots.push(public.join("Desktop"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA").map(PathBuf::from) {
        roots.push(appdata.join("Microsoft/Windows/Start Menu/Programs"));
    }
    if let Some(program_data) = std::env::var_os("ProgramData").map(PathBuf::from) {
        roots.push(program_data.join("Microsoft/Windows/Start Menu/Programs"));
    }
    roots.retain(|path| path.is_dir());
    roots
}

fn is_downloads(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.eq_ignore_ascii_case("Downloads"))
        .unwrap_or(false)
}

// ---------------------------------------------------------------- 清理

/// 删掉所有属于本程序的历史入口（含规范名，随后重建），返回删除数量。
///
/// 判据：文件名里带 `aicrm`，或 `.lnk` 内容里出现本安装目录 —— 前者覆盖旧版本
/// 留下的入口，后者覆盖指向旧安装路径的死图标。
fn cleanup_links(roots: &[PathBuf], install_dir: &Path) -> usize {
    let install_dir_text = install_dir
        .to_string_lossy()
        .replace('/', "\\")
        .to_lowercase();
    let mut removed = 0usize;
    for root in roots {
        for link in walk_lnk(root, 3) {
            let name = link
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !name.contains("aicrm") && !refers_to(&link, &install_dir_text) {
                continue;
            }
            if std::fs::remove_file(&link).is_ok() {
                removed += 1;
            }
        }
    }
    removed
}

/// 递归收集 `.lnk`，深度有限 —— 开始菜单里的层级最多也就两三层。
fn walk_lnk(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();
    if depth == 0 {
        return found;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return found,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(walk_lnk(&path, depth - 1));
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .map(|value| value.eq_ignore_ascii_case("lnk"))
            .unwrap_or(false)
        {
            found.push(path);
        }
    }
    found
}

/// `.lnk` 里目标路径是明文（ANSI 或 UTF-16LE）。两种编码都查一遍就够用了。
fn refers_to(path: &Path, needle: &str) -> bool {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(_) => return false,
    };
    let ansi: String = bytes
        .iter()
        .map(|byte| byte.to_ascii_lowercase() as char)
        .collect();
    if ansi.contains(needle) {
        return true;
    }
    let wide: String = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .filter_map(|unit| char::from_u32(unit as u32))
        .collect::<String>()
        .to_lowercase();
    wide.contains(needle)
}

// ---------------------------------------------------------------- 建入口

/// 在桌面与开始菜单写入口，返回成功数量。下载目录不放入口。
fn write_links(roots: &[PathBuf], target: &Path, install_dir: &Path) -> usize {
    let mut created = 0usize;
    for root in roots {
        if is_downloads(root) {
            continue;
        }
        let link = root.join(SHORTCUT_NAME);
        match shell_link(&link, target, install_dir) {
            Ok(()) => created += 1,
            Err(error) => eprintln!("写快捷方式 {} 失败：{error}", link.display()),
        }
    }
    created
}

#[cfg(windows)]
fn shell_link(link: &Path, target: &Path, work_dir: &Path) -> Result<(), String> {
    com::create_shortcut(
        &link.to_string_lossy(),
        &target.to_string_lossy(),
        &work_dir.to_string_lossy(),
    )
}

#[cfg(not(windows))]
fn shell_link(_link: &Path, _target: &Path, _work_dir: &Path) -> Result<(), String> {
    Err("仅 Windows 支持".to_owned())
}

/// `IShellLink` 的最小封装：SetPath / SetWorkingDirectory / SetIconLocation → Save。
///
/// 只用到四个方法，直接按 vtable 顺序取（IUnknown 3 个在前）：
/// 0 QueryInterface、9 SetWorkingDirectory、17 SetIconLocation、20 SetPath；
/// `IPersistFile` 的 6 是 Save。索引是拿可运行样例逐条验证过的，别凭直觉改。
#[cfg(windows)]
mod com {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Guid {
        d1: u32,
        d2: u16,
        d3: u16,
        d4: [u8; 8],
    }

    const CLSID_SHELL_LINK: Guid = Guid {
        d1: 0x0002_1401,
        d2: 0x0000,
        d3: 0x0000,
        d4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };
    const IID_SHELL_LINK_W: Guid = Guid {
        d1: 0x0002_14F9,
        d2: 0x0000,
        d3: 0x0000,
        d4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };
    const IID_PERSIST_FILE: Guid = Guid {
        d1: 0x0000_010B,
        d2: 0x0000,
        d3: 0x0000,
        d4: [0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
    };

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoInitialize(reserved: *mut c_void) -> i32;
        fn CoUninitialize();
        fn CoCreateInstance(
            class: *const Guid,
            outer: *mut c_void,
            context: u32,
            interface: *const Guid,
            out: *mut *mut c_void,
        ) -> i32;
    }

    fn wide(text: &str) -> Vec<u16> {
        std::ffi::OsStr::new(text)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// 取 vtable 第 index 项并转成函数指针。
    unsafe fn slot<F>(object: *mut c_void, index: usize) -> F {
        let table = *(object as *mut *mut usize);
        let entry = *table.add(index);
        unsafe { std::mem::transmute_copy::<usize, F>(&entry) }
    }

    pub fn create_shortcut(link: &str, target: &str, work_dir: &str) -> Result<(), String> {
        unsafe {
            let _ = CoInitialize(std::ptr::null_mut());
            let mut shell: *mut c_void = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_SHELL_LINK,
                std::ptr::null_mut(),
                1, // CLSCTX_INPROC_SERVER
                &IID_SHELL_LINK_W,
                &mut shell,
            );
            if hr < 0 || shell.is_null() {
                return Err(format!("创建 ShellLink 失败：{hr:#010x}"));
            }

            type SetText = unsafe extern "system" fn(*mut c_void, *const u16) -> i32;
            type SetIcon = unsafe extern "system" fn(*mut c_void, *const u16, i32) -> i32;
            type Query = unsafe extern "system" fn(*mut c_void, *const Guid, *mut *mut c_void) -> i32;
            type Release = unsafe extern "system" fn(*mut c_void) -> u32;

            let target_w = wide(target);
            let work_w = wide(work_dir);
            let set_path: SetText = slot(shell, 20);
            let set_work: SetText = slot(shell, 9);
            let set_icon: SetIcon = slot(shell, 17);
            let query: Query = slot(shell, 0);
            let release: Release = slot(shell, 2);

            set_path(shell, target_w.as_ptr());
            set_work(shell, work_w.as_ptr());
            set_icon(shell, target_w.as_ptr(), 0);

            let mut persist: *mut c_void = std::ptr::null_mut();
            let hr = query(shell, &IID_PERSIST_FILE, &mut persist);
            if hr < 0 || persist.is_null() {
                release(shell);
                return Err(format!("取 IPersistFile 失败：{hr:#010x}"));
            }

            let save: SetIcon = slot(persist, 6); // Save(path, remember)
            let link_w = wide(link);
            let hr = save(persist, link_w.as_ptr(), 1);

            let release_persist: Release = slot(persist, 2);
            release_persist(persist);
            release(shell);
            CoUninitialize();

            if hr < 0 {
                return Err(format!("保存快捷方式失败：{hr:#010x}"));
            }
            Ok(())
        }
    }
}

// ---------------------------------------------------------------- 过期安装包

/// 把桌面/下载目录里过期的 `AICRM*.exe` 移进安装目录的 `updates\archive`。
///
/// 只认自己那套命名，绝不动客户的其它文件；而且是**移动**不是删除，随时可拿回。
fn archive_stale_packages(roots: &[PathBuf], install_dir: &Path, log_file: &Path) -> usize {
    let archive = install_dir.join("updates").join("archive");
    let mut moved = 0usize;
    for root in roots {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !name.starts_with("aicrm") || !name.ends_with(".exe") {
                continue;
            }
            let Some(file_name) = path.file_name() else {
                continue;
            };
            let destination = archive.join(file_name);
            if std::fs::create_dir_all(&archive).is_err() {
                continue;
            }
            let _ = std::fs::remove_file(&destination);
            if std::fs::rename(&path, &destination).is_ok() {
                moved += 1;
                log(log_file, &format!("归档过期安装包：{}", path.display()));
            }
        }
    }
    moved
}

/// 让资源管理器立刻重画桌面：删掉的文件残留图标、被替换程序的旧图标都会消失。
fn refresh_icon_cache() {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let _ = Command::new("ie4uinit.exe")
            .arg("-show")
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

pub(crate) fn log(path: &Path, message: &str) {
    let line = format!("[{}] {message}\n", crate::epoch_seconds());
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
}
