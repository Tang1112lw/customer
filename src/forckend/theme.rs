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
/// 状态色：正常 / 异常 / 未知（正常色与图标同一套绿）
pub const OK: egui::Color32 = egui::Color32::from_rgb(0x22, 0xA0, 0x50);
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

// ---- 玻璃质感（2026-10-13，安装器与登录引导页）----
//
// egui 没有背景模糊，玻璃感全靠三层叠出来：浅色灰绿底 × 半透明白层 ×
// 受光高光边。所有色值都带 alpha，层与层之间互相透出一点，才有"玻璃"感。
// 注意：egui 的 from_rgba_unmultiplied 不是 const fn，带 alpha 的色值
// 一律走函数（from_rgb 是 const，不透明的照旧用常量）。

/// 玻璃页面底：比 PAGE 更偏绿的浅灰绿。
pub const GLASS_PAGE: egui::Color32 = egui::Color32::from_rgb(0xE9, 0xF0, 0xEC);
/// 浅绿玻璃侧栏：88% 不透明的薄荷绿，叠在 GLASS_PAGE 上呈通透感。
#[allow(dead_code)] // 仅主控台登录引导页用；安装器 bin 编译同一文件，见文件头说明
pub fn glass_green() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0xC9, 0xE7, 0xDA, 225)
}
/// 绿玻璃上的文字：深绿，白字在浅底上会跳。
#[allow(dead_code)] // 同上：只有登录引导页的侧栏用它
pub const GLASS_GREEN_INK: egui::Color32 = egui::Color32::from_rgb(0x08, 0x50, 0x41);
/// 玻璃卡片：82% 白。
pub fn glass_card_color() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 209)
}
/// 玻璃控件（输入框 / 次按钮）：70% 白。
#[allow(dead_code)] // 仅登录引导页的输入框用；安装器走 primary/ghost_button
pub fn glass_control() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 179)
}
/// 玻璃描边：近白。玻璃世界的"边框"不是灰线，是受光的亮边。
pub fn glass_edge() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 242)
}
/// 玻璃顶部高光：2px 近白，压在卡片上边内侧 —— 玻璃的受光面。
pub fn glass_hilite() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 250)
}
/// 玻璃进度绿：比 OK 鲜一档，在玻璃底上更跳。
#[allow(dead_code)] // 安装器的检测进度条用；主控台 bin 编译同一文件（见文件头说明）
pub const GLASS_OK: egui::Color32 = egui::Color32::from_rgb(0x1D, 0x9E, 0x75);
/// 进度条槽：6% 黑。
#[allow(dead_code)] // 同上：只有安装器的进度条用
pub fn track() -> egui::Color32 {
    egui::Color32::from_rgba_unmultiplied(0x1F, 0x24, 0x30, 26)
}

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

/// 卡片内的细分隔线，用来把"信息区"和"操作区"分开。
pub fn divider(ui: &mut egui::Ui) {
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 1.0), egui::Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, egui::Stroke::new(1.0, LINE));
}

/// 玻璃卡片：半透明白 + 顶部 2px 受光高光边。
///
/// 与 [`card`] 同一套用法（固定内容宽度、强制竖排），只是换成玻璃三件套。
pub fn glass_card<R>(
    ui: &mut egui::Ui,
    width: f32,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let out = egui::Frame::NONE
        .fill(glass_card_color())
        .stroke(egui::Stroke::new(1.0, glass_edge()))
        .corner_radius(12)
        .inner_margin(PAD as i8)
        .show(ui, |ui| {
            ui.set_width(width);
            ui.vertical(|ui| add(ui)).inner
        });
    let rect = out.response.rect;
    if rect.width() > 24.0 {
        let line = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 12.0, rect.top() + 1.0),
            egui::vec2(rect.width() - 24.0, 2.0),
        );
        ui.painter()
            .rect_filled(line, egui::CornerRadius::same(1), glass_hilite());
    }
    out.inner
}

/// 浅绿玻璃侧栏：整幅薄荷玻璃 + 右缘一道白色受光边。
///
/// 侧栏是**整幅纯色**，不再叠任何渐变/光泽层 —— 叠层之间的硬边在纯色上
/// 会切出明暗带（用户反馈的"色彩断层"），玻璃感靠 alpha 本身就够了。
#[allow(dead_code)] // 仅主控台登录引导页用；安装器没有侧栏
pub fn glass_side(ui: &mut egui::Ui, width: f32, height: f32, add: impl FnOnce(&mut egui::Ui)) {
    let out = egui::Frame::NONE
        .fill(glass_green())
        .inner_margin(egui::Margin {
            left: 20,
            right: 12,
            top: 26,
            bottom: 0,
        })
        .show(ui, |ui| {
            ui.set_width(width);
            ui.set_height(height);
            ui.vertical(|ui| add(ui));
        });
    let rect = out.response.rect;
    ui.painter().vline(
        rect.right() - 1.0,
        rect.y_range(),
        egui::Stroke::new(1.0, glass_edge()),
    );
}

/// 玻璃输入框：半透明白底 + 受光边（直接换掉 TextEdit 的默认外框）。
#[allow(dead_code)] // 仅登录引导页用；安装器没有输入框
pub fn glass_input(
    ui: &mut egui::Ui,
    text: &mut String,
    password: bool,
    enabled: bool,
) -> egui::Response {
    ui.add_enabled(
        enabled,
        egui::TextEdit::singleline(text)
            .password(password)
            .frame(
                egui::Frame::NONE
                    .fill(glass_control())
                    .stroke(egui::Stroke::new(1.0, glass_edge()))
                    .corner_radius(8)
                    .inner_margin(egui::Margin::symmetric(10, 8)),
            )
            .desired_width(ui.available_width()),
    )
}

/// 玻璃进度条：6% 黑槽 + 实色填充。
///
/// `ratio = None` 时画一段来回扫描的亮条（检测中，没有真实进度可报）——
/// 调用方负责在这期间 `request_repaint`。
#[allow(dead_code)] // 安装器的环境检测用；主控台 bin 编译同一文件
pub fn glass_progress(ui: &mut egui::Ui, width: f32, ratio: Option<f32>, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 10.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(5), track());
    match ratio {
        Some(value) => {
            let fill = rect.width() * value.clamp(0.0, 1.0);
            if fill > 0.5 {
                ui.painter().rect_filled(
                    egui::Rect::from_min_size(rect.min, egui::vec2(fill, rect.height())),
                    egui::CornerRadius::same(5),
                    color,
                );
            }
        }
        None => {
            let phase = ((ui.input(|input| input.time) % 1.4) / 1.4) as f32;
            let segment = 0.3_f32;
            let x = rect.left() + rect.width() * (1.0 - segment) * phase;
            ui.painter().rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(rect.width() * segment, rect.height()),
                ),
                egui::CornerRadius::same(5),
                color,
            );
        }
    }
}

/// 玻璃按钮：与 [`glass_card`] 同一套表单（半透明白 + 顶部受光高光边 + 深色字）。
///
/// 安装器的「开始安装 / 重新检测」用它——两个按钮必须**完全同色**，
/// 主次只靠位置与文字表达，不靠颜色区分（用户明确要求）。
#[allow(dead_code)] // 安装器的操作用；主控台 bin 编译同一文件
pub fn glass_button(ui: &mut egui::Ui, width: f32, text: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 44.0), egui::Sense::click());
    let fill = if response.hovered() {
        egui::Color32::from_rgba_unmultiplied(0xFF, 0xFF, 0xFF, 235)
    } else {
        glass_control()
    };
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(10), fill);
    let line = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 10.0, rect.top() + 1.0),
        egui::vec2(rect.width() - 20.0, 2.0),
    );
    ui.painter()
        .rect_filled(line, egui::CornerRadius::same(1), glass_hilite());
    ui.painter().rect_stroke(
        rect,
        egui::CornerRadius::same(10),
        egui::Stroke::new(1.0, glass_edge()),
        egui::StrokeKind::Inside,
    );
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_owned(), egui::FontId::proportional(14.0), TEXT);
    ui.painter()
        .galley(rect.center() - galley.size() * 0.5, galley, TEXT);
    response
}

/// 玻璃风状态徽标：实心圆 + 手绘对勾。
///
/// 不用字体里的 "✓" 字形 —— CJK 字体覆盖没保证，缺字形时 egui 会画豆腐块。
#[allow(dead_code)] // 安装器的检测结果徽标用；主控台 bin 编译同一文件
pub fn check_badge(ui: &mut egui::Ui, color: egui::Color32, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let center = rect.center();
    let radius = size / 2.0;
    ui.painter().circle_filled(center, radius, color);
    let stroke = egui::Stroke::new((size * 0.11).max(1.5), egui::Color32::WHITE);
    ui.painter().line_segment(
        [
            center + egui::vec2(-radius * 0.45, -radius * 0.02),
            center + egui::vec2(-radius * 0.08, radius * 0.36),
        ],
        stroke,
    );
    ui.painter().line_segment(
        [
            center + egui::vec2(-radius * 0.08, radius * 0.36),
            center + egui::vec2(radius * 0.48, -radius * 0.38),
        ],
        stroke,
    );
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
