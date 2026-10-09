//! 打包工具：把 `aicrm-setup.exe` 和全部载荷拼成**单文件安装程序**。
//!
//! 产物结构（从前往后）：
//!
//! ```text
//! [aicrm-setup.exe 本体][载荷 zip][offset u64][len u64]["AICRMSFX"]
//! ```
//!
//! 安装器启动时读自身末尾 24 字节拿到 offset/len，定位并解压载荷到临时目录，
//! 再跑里面的 `install.ps1`。所以客户只需要拿到**一个 exe**，双击即可。
//!
//! 用法：
//!
//! ```text
//! cargo build --release
//! cargo run --release --bin aicrm-pack [输出路径]
//! ```
//!
//! 默认输出 `target/release/AICRM-客户端安装程序.exe`。

use std::{
    fs,
    io::{self, Seek, Write},
    path::{Path, PathBuf},
};

/// 页脚魔数，安装器靠它确认"这确实是一个完整安装包"。
const MAGIC: &[u8; 8] = b"AICRMSFX";

/// 载荷布局：仓库里的脚本放根目录，编译产物和辅助程序各就各位。
/// 少一个文件就直接报错 —— 装到一半才发现缺东西是最糟的情况。
const SCRIPTS: [&str; 2] = ["install.ps1", "uninstall.ps1"];
const BINARIES: [&str; 3] = [
    "aicrm_windows_api.exe",
    "aicrm-login.exe",
    "aicrm-console.exe",
];
const TOOLS: [&str; 3] = [
    "AICRM-Key.exe",
    "aicrm-export-new.exe",
    "使用手册.txt",
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let release = root.join("target").join("release");
    let stub = release.join("aicrm-setup.exe");
    if !stub.is_file() {
        return Err(format!(
            "找不到 {}：先跑 cargo build --release 把安装器本体编出来",
            stub.display()
        )
        .into());
    }

    let output = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| release.join("AICRM-客户端安装程序.exe"));

    // ① 组载荷 zip
    let mut payload: Vec<u8> = Vec::new();
    {
        let mut writer = zip::ZipWriter::new(io::Cursor::new(&mut payload));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let mut total: u64 = 0;

        for name in SCRIPTS {
            add(&mut writer, options, &root.join(name), name, &mut total)?;
        }
        for name in BINARIES {
            add(&mut writer, options, &release.join(name), name, &mut total)?;
        }
        for name in TOOLS {
            add(
                &mut writer,
                options,
                &root.join("src").join("tools").join(name),
                &format!("tools/{name}"),
                &mut total,
            )?;
        }
        writer.finish()?;
        let files = SCRIPTS.len() + BINARIES.len() + TOOLS.len();
        println!("载荷：{files} 个文件，原始 {total} 字节 → 压缩后 {} 字节", payload.len());
    }

    // ② 拼出成品
    let mut output_file = fs::File::create(&output)?;
    io::copy(&mut fs::File::open(&stub)?, &mut output_file)?;
    let offset = output_file.stream_position()?;
    output_file.write_all(&payload)?;
    output_file.write_all(&offset.to_le_bytes())?;
    output_file.write_all(&(payload.len() as u64).to_le_bytes())?;
    output_file.write_all(MAGIC)?;
    output_file.sync_all()?;
    drop(output_file);

    // ③ 自检：按安装器的读法把页脚再解析一遍，确认能定位到载荷
    let written = fs::read(&output)?;
    let size = written.len() as u64;
    let footer = &written[written.len() - 24..];
    if &footer[16..] != MAGIC {
        return Err("自检失败：页脚魔数不对".into());
    }
    let read_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
    let read_len = u64::from_le_bytes(footer[8..16].try_into().unwrap());
    if read_offset != offset || read_len != payload.len() as u64 || read_offset + read_len + 24 != size
    {
        return Err("自检失败：页脚记录的偏移/长度对不上".into());
    }

    println!(
        "✅ 单文件安装程序已生成：{}\n   总大小 {} 字节（本体 {} + 载荷 {} + 页脚 24）",
        output.display(),
        size,
        offset,
        read_len
    );
    Ok(())
}

/// 往 zip 里塞一个文件，顺带累计原始大小。
fn add(
    writer: &mut zip::ZipWriter<io::Cursor<&mut Vec<u8>>>,
    options: zip::write::SimpleFileOptions,
    path: &Path,
    name: &str,
    total: &mut u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = fs::read(path).map_err(|error| format!("读不到 {}：{error}", path.display()))?;
    *total += data.len() as u64;
    writer.start_file(name, options)?;
    writer.write_all(&data)?;
    Ok(())
}
