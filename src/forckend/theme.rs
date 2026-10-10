//! 两个窗口（主控台、安装器）共用的外观：浅色底、白卡片、描边代替阴影、留白放宽。
//!
//! 每个 GUI 程序都是独立二进制，各自 `mod theme;` 指向同一份文件 ——
//! 不引入共享库，跟这个包"服务侧不链接 GUI 依赖"的取舍保持一致。

use eframe::egui;

/// 页面底色 / 卡片底色 / 描边
pub const PAGE: egui::Color32 = egui::Color32::from_rgb(0xF4, 0xF5, 0xF7);
pub const CARD: egui::Color32 = egui::Color32::from_rgb(0xFF, 0xFF, 0xFF);
pub const LINE: egui::Color32 = egui::Color32::from_rgb(0xE5, 0xE7, 0xEB);
/// 文字：正文 / 次要 / 弱
pub const TEXT: egui::Color32 = egui::Color32::from_rgb(0x1F, 0x24, 0x30);
pub const MUTED: egui::Color32 = egui::Color32::from_rgb(0x77, 0x7E, 0x8C);
/// 状态色：正常 / 待办 / 异常 / 未知（正常色与图标同一套绿）
pub const OK: egui::Color32 = egui::Color32::from_rgb(0x22, 0xA0, 0x50);
pub const WARN: egui::Color32 = egui::Color32::from_rgb(0xD1, 0x7A, 0x06);
pub const BAD: egui::Color32 = egui::Color32::from_rgb(0xD6, 0x3A, 0x3A);
pub const IDLE: egui::Color32 = egui::Color32::from_rgb(0x9C, 0xA3, 0xAF);
/// 强调色：只给"主操作"用，全局出现次数越少越有分量
pub const ACCENT: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x6F, 0xED);
/// 更浅的提示文字，用于"每 5 秒自动刷新"这类几乎不需要读的信息
pub const FAINT: egui::Color32 = egui::Color32::from_rgb(0xA8, 0xAE, 0xB8);
/// 侧边栏底色 / 悬停底色
pub const SIDEBAR: egui::Color32 = egui::Color32::from_rgb(0xF7, 0xF8, 0xFA);
pub const SIDEBAR_HOVER: egui::Color32 = egui::Color32::from_rgb(0xEE, 0xF0, 0xF4);
/// 内容区底色：比侧边栏再亮一档，让两块区域不用描边也能分开
pub const CONTENT: egui::Color32 = egui::Color32::from_rgb(0xFB, 0xFB, 0xFC);

/// 统一外观。字体由各窗口自己的 `install_cjk_font` 负责，这里只管配色与间距。
pub fn apply(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::light();
    visuals.panel_fill = PAGE;
    visuals.window_fill = PAGE;
    visuals.extreme_bg_color = CARD;
    visuals.override_text_color = Some(TEXT);
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, LINE);
    visuals.widgets.inactive.weak_bg_fill = CARD;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, LINE);
    visuals.widgets.hovered.weak_bg_fill = egui::Color32::from_rgb(0xF0, 0xF2, 0xF5);
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(0xD5, 0xD9, 0xE0));
    visuals.widgets.active.weak_bg_fill = egui::Color32::from_rgb(0xEA, 0xED, 0xF2);
    ctx.set_visuals(visuals);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
    });
}

/// 卡片内边距（卡片总宽 = 内容宽 + PAD×2，布局算宽度时要用）。
pub const PAD: f32 = 14.0;

/// 一张卡片：白底 + 1px 描边 + 圆角 + 固定内容宽度。
///
/// 内容**强制自上而下**：`Frame` 内部的 Ui 会沿用父级布局，而卡片通常被放在
/// `ui.horizontal()` 的横向行里 —— 不显式 `vertical` 的话，卡里所有内容会横着
/// 排成一行并把卡片撑到整屏宽（踩过）。
pub fn card<R>(ui: &mut egui::Ui, width: f32, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::NONE
        .fill(CARD)
        .stroke(egui::Stroke::new(1.0, LINE))
        .corner_radius(10)
        .inner_margin(PAD as i8)
        .show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| add(ui)).inner
        })
        .inner
}

/// 小节标题（"最近采集"这类）。
pub fn section(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(13.0).strong().color(MUTED)
}

/// 卡片里的主值：比正文重一档，用来承载"这条卡片到底在说什么"。
pub fn value(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(15.0).strong().color(TEXT)
}

/// 次要说明文字。
pub fn muted(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(12.0).color(MUTED)
}

/// 最弱的提示文字（"每 5 秒自动刷新"这类）。
pub fn faint(text: &str) -> egui::RichText {
    egui::RichText::new(text).size(12.0).color(FAINT)
}

/// 状态胶囊：同色系的淡底 + 实色字。
///
/// 比"圆点 + 灰字"更容易一眼扫到 —— 8px 的小点在窗口里太不显眼，
/// 且和标题同色，读的时候要刻意去找。
pub fn pill(ui: &mut egui::Ui, color: egui::Color32, text: &str) {
    egui::Frame::NONE
        .fill(egui::Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            26,
        ))
        .corner_radius(20)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(color));
        });
}

/// 卡片首行：左边胶囊状态，右边标题。
///
/// 状态放左是有意的 —— 扫一列卡片时视线沿左侧垂直下移，状态在左上角才扫得到；
/// 放右上角会随卡片宽度浮动，视线要左右横跳。
pub fn card_head_rev(ui: &mut egui::Ui, label: &str, color: egui::Color32, status: &str) {
    ui.horizontal(|ui| {
        pill(ui, color, status);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(egui::RichText::new(label).size(12.0).color(MUTED));
        });
    });
}

/// 卡片内的细分隔线，用来把"信息区"和"操作区"分开。
pub fn divider(ui: &mut egui::Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, LINE));
}

/// 主操作按钮：填充强调色。一屏里最多出现一个。
pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(13.0).color(egui::Color32::WHITE))
            .fill(ACCENT)
            .corner_radius(6),
    )
}

/// 次操作按钮：白底描边。卡片里的常规动作都用它。
pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(13.0).color(TEXT))
            .fill(CARD)
            .stroke(egui::Stroke::new(1.0, LINE))
            .corner_radius(6),
    )
}

/// 危险动作按钮：停止服务这类，用红字白底，避免和普通操作混淆。
pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(egui::RichText::new(text).size(13.0).color(BAD))
            .fill(CARD)
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(0xF0, 0xD4, 0xD4)))
            .corner_radius(6),
    )
}

/// 侧边栏导航项。整行可点；选中态是白底 + 左侧 3px 蓝条。
///
/// 用 `allocate_exact_size` 手绘而不是 `Button`：左边那条竖线用 Button 表达不出来，
/// 而它正是"当前在哪一页"唯一的视觉锚点。
pub fn nav_item(ui: &mut egui::Ui, text: &str, active: bool) -> bool {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 34.0), egui::Sense::click());

    let fill = if active {
        CARD
    } else if response.hovered() {
        SIDEBAR_HOVER
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(8), fill);

    if active {
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 1.0, rect.top() + 7.0),
            egui::vec2(3.0, 20.0),
        );
        ui.painter()
            .rect_filled(bar, egui::CornerRadius::same(2), ACCENT);
    }

    ui.painter().text(
        egui::pos2(rect.left() + 15.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(13.0),
        if active { ACCENT } else { MUTED },
    );
    response.clicked()
}
