//! egui 中文字体加载，主控台与安装器共用。
//!
//! **不要放进 `shared/mod.rs`**：本文件依赖 eframe/egui，只允许 GUI 二进制
//! 通过 `#[path = "../shared/font.rs"] mod font;` 单独引入 —— 把 GUI 依赖
//! 牵进后台服务是刻意避免的（服务侧不链接界面依赖）。

use eframe::egui;
use std::sync::Arc;

/// egui 自带字体不含 CJK 字形。不加载中文字体的话，整个界面会显示成方框。
pub fn install_cjk_font(ctx: &egui::Context) {
    const CANDIDATES: [&str; 4] = [
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
    ];
    let Some(bytes) = CANDIDATES.iter().find_map(|path| std::fs::read(path).ok()) else {
        return;
    };

    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "cjk".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes)),
    );
    // 追加而不是插到最前：拉丁字符继续用自带字体，汉字回退到中文字体。
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("cjk".to_owned());
    }
    ctx.set_fonts(fonts);
}
