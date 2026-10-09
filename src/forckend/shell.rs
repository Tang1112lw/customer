//! 通知区托盘 —— 主窗口的常驻外壳。
//!
//! **为什么不单独做一个进程**：托盘的职责就是"把主窗口找回来"，它必须和窗口
//! 共享同一份状态。拆成两个进程就得再造一套 IPC，还会出现"托盘在、窗口早死了"
//! 的错乱 —— 客户看到图标点不动，只能去任务管理器杀进程。合成一个进程，窗口
//! 隐藏只是把视口设为不可见，托盘和状态都还在。
//!
//! 交互沿用主流客户端（微信 / Slack / Discord）的惯例：
//! 左键开关窗口，右键弹菜单，真正的退出只在菜单里。

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use eframe::egui;
use tray_icon::{
    MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};

/// 托盘能触发的动作。
#[derive(Clone, Copy)]
pub enum Action {
    /// 切换主窗口显示 / 隐藏（左键单击与菜单项都走这个）
    Toggle,
    StartService,
    StopService,
    /// 完全退出：连本地服务等子进程一起收掉，不再留后台进程
    Quit,
}

pub struct Tray {
    icon: TrayIcon,
    status: MenuItem,
    toggle: MenuItem,
    start: MenuItem,
    stop: MenuItem,
    quit: MenuItem,
    /// 事件中转队列，见 [`Tray::new`] 里那个后台线程
    inbox: Arc<Mutex<VecDeque<Action>>>,
    /// 上次写入的状态：菜单文本和图标只在变化时重设，否则每帧刷新会让菜单闪烁
    running: Option<bool>,
    visible: bool,
}

impl Tray {
    pub fn new(ctx: &egui::Context) -> Result<Self, Box<dyn std::error::Error>> {
        let status = MenuItem::new("服务：检查中…", false, None);
        let toggle = MenuItem::new("打开主界面", true, None);
        let start = MenuItem::new("启动服务", true, None);
        let stop = MenuItem::new("停止服务", true, None);
        let quit = MenuItem::new("退出（同时停止服务）", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &toggle,
            &start,
            &stop,
            &PredefinedMenuItem::separator(),
            &quit,
        ])?;

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("AICRM 客户端")
            .with_icon(status_icon(false))
            // 左键留给"开关窗口"，菜单只在右键时出现
            .with_menu_on_left_click(false)
            .build()?;

        let tray = Self {
            icon,
            status,
            toggle,
            start,
            stop,
            quit,
            inbox: Arc::new(Mutex::new(VecDeque::new())),
            running: None,
            visible: true,
        };
        tray.spawn_listener(ctx);
        Ok(tray)
    }

    /// 后台线程收托盘事件，顺手把界面唤醒。
    ///
    /// **为什么不在绘制线程里直接收**：窗口隐藏后 egui 会停止重绘、事件循环休眠，
    /// 没人去读事件 —— 客户点托盘就会毫无反应，正好是最要命的那一下。这里用独立
    /// 线程收，收到就 `request_repaint` 把界面叫醒，再把动作放进队列交给界面执行。
    fn spawn_listener(&self, ctx: &egui::Context) {
        let ids = (
            self.toggle.id().clone(),
            self.start.id().clone(),
            self.stop.id().clone(),
            self.quit.id().clone(),
        );
        let sink = self.inbox.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || loop {
            let mut got: VecDeque<Action> = VecDeque::new();

            // 左键抬起 = 开关窗口（用抬起而不是按下，避免长按拖动时误触发）
            while let Ok(event) = TrayIconEvent::receiver().try_recv() {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    got.push_back(Action::Toggle);
                }
            }
            while let Ok(event) = MenuEvent::receiver().try_recv() {
                let id = event.id();
                let action = if ids.0 == id {
                    Some(Action::Toggle)
                } else if ids.1 == id {
                    Some(Action::StartService)
                } else if ids.2 == id {
                    Some(Action::StopService)
                } else if ids.3 == id {
                    Some(Action::Quit)
                } else {
                    None
                };
                got.extend(action);
            }

            if !got.is_empty() {
                if let Ok(mut queue) = sink.lock() {
                    queue.extend(got);
                }
                ctx.request_repaint();
            }
            std::thread::sleep(Duration::from_millis(100));
        });
    }

    /// 取一个待办动作，顺便刷新菜单状态。要在每帧调用一次。
    pub fn poll(&mut self, service_up: bool, visible: bool) -> Option<Action> {
        let action = self.inbox.lock().ok().and_then(|mut queue| queue.pop_front());

        if self.running != Some(service_up) {
            self.running = Some(service_up);
            self.status.set_text(if service_up {
                "服务：运行中"
            } else {
                "服务：已停止"
            });
            self.start.set_enabled(!service_up);
            self.stop.set_enabled(service_up);
            let _ = self.icon.set_icon(Some(status_icon(service_up)));
        }
        if self.visible != visible {
            self.visible = visible;
            self.toggle.set_text(if visible {
                "隐藏主界面"
            } else {
                "打开主界面"
            });
        }
        action
    }
}

/// 通知区图标：圆角方块 + 中心圆点，绿 = 服务运行中，灰 = 已停止。
///
/// 画在代码里而不是读 .ico，是为了让颜色直接表达服务状态 ——
/// 客户不用点开菜单就知道服务在不在。
fn status_icon(running: bool) -> tray_icon::Icon {
    /// 留白 / 圆角半径 / 中心圆点半径，都按边长的比例取。
    const INSET: f32 = 0.06;
    const CORNER: f32 = 0.22;
    const DOT: f32 = 0.18;
    const SIZE: u32 = 32;

    let (red, green, blue) = if running {
        (34, 160, 80)
    } else {
        (128, 128, 128)
    };
    let side = SIZE as f32;
    let center = side / 2.0;
    let half = center - side * INSET;
    let corner = side * CORNER;
    let inner = half - corner;
    let dot = side * DOT;

    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            // 采样像素中心，配合下面的覆盖率做抗锯齿
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;

            // 圆角矩形的有符号距离
            let qx = (px - center).abs() - inner;
            let qy = (py - center).abs() - inner;
            let (mx, my) = (qx.max(0.0), qy.max(0.0));
            let tile = (mx * mx + my * my).sqrt() + qx.max(qy).min(0.0) - corner;

            // 中心圆点的有符号距离
            let dot_distance = ((px - center).powi(2) + (py - center).powi(2)).sqrt() - dot;

            let tile_alpha = coverage(tile);
            // 圆点只在方块范围内混色
            let dot_alpha = coverage(dot_distance) * tile_alpha;

            let offset = ((y * SIZE + x) * 4) as usize;
            rgba[offset] = mix(red, 255, dot_alpha);
            rgba[offset + 1] = mix(green, 255, dot_alpha);
            rgba[offset + 2] = mix(blue, 255, dot_alpha);
            rgba[offset + 3] = (tile_alpha * 255.0).round() as u8;
        }
    }
    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("32x32 RGBA 图标尺寸固定合法")
}

/// 有符号距离 → 覆盖率：边缘 1px 内线性过渡，起抗锯齿作用。
fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

/// 在底色上叠一层白色，`alpha` 为覆盖比例。
fn mix(base: u8, top: u8, alpha: f32) -> u8 {
    (base as f32 * (1.0 - alpha) + top as f32 * alpha).round() as u8
}
