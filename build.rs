//! 构建脚本：把 `assets/aicrm.ico` 作为 Windows 资源嵌进分发给客户的每个程序，
//! 这样资源管理器/任务栏里显示的就是正式图标，而不是空白通用图标。
//!
//! 为什么手写 `.res` 而不引 `winres` / `embed-resource`：这个包一直刻意压依赖
//! （连 HTTP 都挑了 ureq），而 `.res` 的格式只有一页纸，为它多一个编译期依赖不划算。
//!
//! 用 `rustc-link-arg-bin` 而非全局 `rustc-link-arg` 逐目标施加，
//! 测试等其它产物不受影响。

use std::{env, fs, path::PathBuf};

/// 资源类型：图标本体 / 图标组（exe 的文件图标取的是图标组）。
const RT_ICON: u32 = 3;
const RT_GROUP_ICON: u32 = 14;

/// 一个尺寸的图标原始数据。
struct Image {
    width: u8,
    height: u8,
    planes: u16,
    bit_count: u16,
    data: Vec<u8>,
}

fn main() {
    println!("cargo:rerun-if-changed=assets/aicrm.ico");

    // 目标不是 Windows 时没有 PE 资源这回事。
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let ico = root.join("assets").join("aicrm.ico");
    let bytes = match fs::read(&ico) {
        Ok(bytes) => bytes,
        Err(error) => {
            // 只警告不失败：没有图标不该让整个构建挂掉。
            println!(
                "cargo:warning=读不到 {}（{error}），分发的程序将没有文件图标",
                ico.display()
            );
            return;
        }
    };

    let images = match parse_ico(&bytes) {
        Ok(images) => images,
        Err(error) => {
            println!("cargo:warning=解析 {} 失败：{error}", ico.display());
            return;
        }
    };
    if images.is_empty() {
        println!("cargo:warning={} 里没有图像", ico.display());
        return;
    }

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR")).join("aicrm-icon.res");
    if let Err(error) = fs::write(&out, build_res(&images)) {
        println!("cargo:warning=写出 {} 失败：{error}", out.display());
        return;
    }

    // 每个会被分发出去的程序都带上图标（打包工具 aicrm-pack 不分发，不需要）。
    // 用 rustc-link-arg-bin 逐目标施加，避免影响测试等其它产物。
    for bin in ["aicrm_windows_api", "aicrm-console", "aicrm-setup"] {
        println!("cargo:rustc-link-arg-bin={bin}={}", out.display());
    }
}

/// 拆 ICO 容器：取每个尺寸的宽高、色深和原始数据（BMP 或 PNG 载荷都原样透传）。
fn parse_ico(bytes: &[u8]) -> Result<Vec<Image>, String> {
    if bytes.len() < 6 {
        return Err("文件太短".to_owned());
    }
    if u16::from_le_bytes([bytes[2], bytes[3]]) != 1 {
        return Err("不是图标文件（type != 1）".to_owned());
    }
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if bytes.len() < 6 + 16 * count {
        return Err("目录不完整".to_owned());
    }

    let mut images = Vec::with_capacity(count);
    for index in 0..count {
        let entry = &bytes[6 + index * 16..6 + index * 16 + 16];
        let size = u32::from_le_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize;
        let offset = u32::from_le_bytes([entry[12], entry[13], entry[14], entry[15]]) as usize;
        let end = offset.checked_add(size).ok_or("偏移溢出")?;
        if end > bytes.len() {
            return Err(format!("第 {index} 张图超出文件末尾"));
        }
        images.push(Image {
            width: entry[0],
            height: entry[1],
            planes: u16::from_le_bytes([entry[4], entry[5]]),
            bit_count: u16::from_le_bytes([entry[6], entry[7]]),
            data: bytes[offset..end].to_vec(),
        });
    }
    Ok(images)
}

/// 组装 `.res`：开头一条空记录，然后 N 条 RT_ICON + 1 条 RT_GROUP_ICON（ID 从 1 开始）。
///
/// 这里的编码细节是拿 `rc.exe` 的输出逐字节对出来的，别凭直觉改：
/// - 每个整数 ID 存成 `(id << 16) | 0xFFFF`（低 16 位全 1 表示"这是序号"），
///   直接写 `3` 或 `1` 会让 link.exe 报 LNK1136「invalid or corrupt file」；
/// - 文件开头必须有一条 DataSize=0 的空资源头，rc.exe 也写；
/// - ICON 记录用 MemoryFlags 0x1010，GROUP_ICON 用 0x1030。
fn build_res(images: &[Image]) -> Vec<u8> {
    let mut out = Vec::new();

    // 开头的空记录
    out.extend_from_slice(&0u32.to_le_bytes()); // DataSize
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&0x0000_FFFFu32.to_le_bytes()); // TypeID：序号 0
    out.extend_from_slice(&0x0000_FFFFu32.to_le_bytes()); // NameID：序号 0
    out.extend_from_slice(&[0u8; 16]); // DataVersion / MemoryFlags / LanguageId / Version / Characteristics

    for (index, image) in images.iter().enumerate() {
        write_resource(&mut out, RT_ICON, index as u32 + 1, 0x1010, &image.data);
    }

    // GRPICONDIR：表头 6 字节 + 每张图 14 字节
    let mut group = Vec::new();
    group.extend_from_slice(&0u16.to_le_bytes()); // 保留位
    group.extend_from_slice(&1u16.to_le_bytes()); // 类型 = 图标
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (index, image) in images.iter().enumerate() {
        group.push(image.width);
        group.push(image.height);
        group.push(0); // 调色板颜色数（32 位无调色板）
        group.push(0); // 保留位
        group.extend_from_slice(&image.planes.to_le_bytes());
        group.extend_from_slice(&image.bit_count.to_le_bytes());
        group.extend_from_slice(&(image.data.len() as u32).to_le_bytes());
        group.extend_from_slice(&((index + 1) as u16).to_le_bytes());
    }
    write_resource(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    out
}

/// 写一条资源记录：32 字节头 + 数据，数据尾部补齐到 4 字节边界。
fn write_resource(out: &mut Vec<u8>, type_id: u32, name_id: u32, memory_flags: u16, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes()); // DataSize
    out.extend_from_slice(&32u32.to_le_bytes()); // HeaderSize
    out.extend_from_slice(&((type_id << 16) | 0xFFFF).to_le_bytes()); // TypeID
    out.extend_from_slice(&((name_id << 16) | 0xFFFF).to_le_bytes()); // NameID
    out.extend_from_slice(&0u32.to_le_bytes()); // DataVersion
    out.extend_from_slice(&memory_flags.to_le_bytes()); // MemoryFlags
    out.extend_from_slice(&0x0409u16.to_le_bytes()); // LanguageId（en-US）
    out.extend_from_slice(&0u32.to_le_bytes()); // Version
    out.extend_from_slice(&0u32.to_le_bytes()); // Characteristics
    out.extend_from_slice(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}
