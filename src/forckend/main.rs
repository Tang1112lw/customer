//! AICRM 登录窗口（Windows 客户端）。
//!
//! 与后台服务 `aicrm_windows_api.exe` 分开的图形程序，职责只有一件：
//!
//! ```text
//! 用户输入账号密码 → 云端校验 → 云端签发会话令牌 → 写进本地令牌文件
//! ```
//!
//! 后台服务随后读该令牌做鉴权。关键点：**令牌由云端签发，所以云端不需要另行"领取"它**
//! —— 云端只要在自己的库里记下"签发给设备 X 的令牌是 Y"即可。
//!
//! 注意：登录窗口**不校验密码**。密码只在内存里停留到请求发出为止，校验完全由云端完成。
//! 任何形式的本地校验都等于在客户端藏一份密码哈希，可被替换或绕过。
//!
//! 用法：
//! ```text
//! aicrm-login.exe --endpoint https://api.example.com/api/login [--token-file 路径] [--device-file 路径]
//! ```

// release 版不弹控制台窗口；debug 版保留，方便看日志。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cloud;
mod store;
// 共用外观模块：这个窗口只用到 apply，其余是给主窗口的
#[allow(dead_code)]
mod theme;

use cloud::{LoginError, Session};
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, TryRecvError},
    },
};

/// 默认指向本机 TangHang 后端的登录接口；正式环境用 `--endpoint` 指定云端 HTTPS 地址。
const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8000/api/auth/login";

fn main() -> eframe::Result {
    let config = Config::from_args();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([360.0, 228.0])
            .with_resizable(false)
            .with_title("AICRM 登录"),
        ..Default::default()
    };
    eframe::run_native(
        "AICRM 登录",
        options,
        Box::new(move |cc| {
            install_cjk_font(&cc.egui_ctx);
            theme::apply(&cc.egui_ctx);
            Ok(Box::new(LoginApp::new(config)))
        }),
    )
}

struct Config {
    endpoint: String,
    token_file: PathBuf,
    device_file: PathBuf,
}

impl Config {
    fn from_args() -> Self {
        let mut endpoint = DEFAULT_ENDPOINT.to_owned();
        let mut token_file = store::default_token_file();
        let mut device_file = store::default_device_file();

        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            if flag == "--help" || flag == "-h" {
                println!(
                    "AICRM 登录窗口\n\
                     \n\
                     --endpoint URL     云端登录接口（默认 {DEFAULT_ENDPOINT}）\n\
                     --token-file PATH  会话令牌写入位置\n\
                     --device-file PATH 设备标识文件位置"
                );
                std::process::exit(0);
            }
            let value = args.next().unwrap_or_default();
            match flag.as_str() {
                "--endpoint" => endpoint = value,
                "--token-file" => token_file = PathBuf::from(value),
                "--device-file" => device_file = PathBuf::from(value),
                other => eprintln!("忽略未知参数：{other}"),
            }
        }

        Self {
            endpoint,
            token_file,
            device_file,
        }
    }
}

/// egui 自带字体不含 CJK 字形。不加载中文字体的话，整个界面会显示成方框。
fn install_cjk_font(ctx: &egui::Context) {
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

enum State {
    Idle,
    Submitting,
    Succeeded {
        display_name: Option<String>,
        warning: Option<String>,
    },
    Failed(String),
}

struct LoginApp {
    config: Config,
    device_id: String,
    username: String,
    password: String,
    state: State,
    pending: Option<Receiver<Result<Session, LoginError>>>,
}

impl LoginApp {
    fn new(config: Config) -> Self {
        let device_id = store::device_id(&config.device_file);
        Self {
            config,
            device_id,
            username: String::new(),
            password: String::new(),
            state: State::Idle,
            pending: None,
        }
    }

    fn busy(&self) -> bool {
        matches!(self.state, State::Submitting | State::Succeeded { .. })
    }

    /// 把账号密码交给后台线程去云端换令牌。
    ///
    /// 密码 `move` 进线程，线程结束后随之析构 —— 不落盘、不留在 UI 状态里。
    fn submit(&mut self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        let username = self.username.trim().to_owned();
        if username.is_empty() || self.password.is_empty() {
            self.state = State::Failed("请填写账号和密码".to_owned());
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let endpoint = self.config.endpoint.clone();
        let device_id = self.device_id.clone();
        let password = std::mem::take(&mut self.password);
        let ctx = ctx.clone();

        std::thread::spawn(move || {
            let outcome = cloud::login(&endpoint, &username, &password, &device_id);
            let _ = sender.send(outcome);
            // 唤醒 UI 线程：否则窗口不会重绘，用户看不到结果
            ctx.request_repaint();
        });

        self.pending = Some(receiver);
        self.state = State::Submitting;
    }

    fn poll(&mut self) {
        let Some(receiver) = &self.pending else {
            return;
        };
        let outcome = match receiver.try_recv() {
            Ok(outcome) => outcome,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                self.state = State::Failed("登录线程异常退出".to_owned());
                return;
            }
        };
        self.pending = None;

        self.state = match outcome {
            Ok(session) => {
                // 账号名单独落盘：主界面右上角要显示"谁登录了"，而令牌文件里只有 JWT，没有账号。
                if let Some(name) = session.display_name.as_deref() {
                    let _ = store::save_account(&store::default_account_file(), name);
                }
                match store::save_token(&self.config.token_file, &session.access_token) {
                    Ok(warning) => State::Succeeded {
                        display_name: session.display_name,
                        warning,
                    },
                    Err(error) => State::Failed(format!("令牌写入失败：{error}")),
                }
            }
            // 失败时保留已输入的账号，但密码已在线程里析构，需要重输。
            Err(error) => State::Failed(error.message().to_owned()),
        };
    }

    fn status_ui(&self, ui: &mut egui::Ui) {
        match &self.state {
            State::Idle => {
                ui.weak("请输入账号和密码");
            }
            State::Submitting => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("正在登录…");
                });
            }
            State::Succeeded {
                display_name,
                warning,
            } => {
                let who = display_name.as_deref().unwrap_or("已登录");
                ui.colored_label(
                    egui::Color32::from_rgb(0, 140, 60),
                    format!("登录成功：{who}"),
                );
                ui.weak("已写入本地令牌 · 服务需重启后生效");
                if let Some(warning) = warning {
                    ui.colored_label(egui::Color32::from_rgb(190, 120, 0), warning);
                }
            }
            State::Failed(message) => {
                ui.colored_label(egui::Color32::from_rgb(190, 50, 50), message);
            }
        }
    }
}

impl eframe::App for LoginApp {
    /// 收后台线程的登录结果。
    ///
    /// 必须放在 `logic` 而不是 `ui`：eframe 0.36 在窗口被遮挡 / 最小化时**只调用 `logic`**，
    /// 完全不跑 egui pass。若把轮询写在 `ui` 里，用户在等待期间最小化窗口，登录结果就永远
    /// 不会被处理、令牌永远不落盘。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let before = std::mem::discriminant(&self.state);
        self.poll();
        // 两种情况都要再要一帧：
        // 1. 提交中 —— 让 spinner 转起来，也让结果一到就能被处理；
        // 2. 状态刚发生变化 —— eframe 0.36 在窗口未聚焦时可能只跑 `logic` 不跑 `ui`，
        //    若不补这一帧，「登录成功」就永远不会被画出来（令牌其实已经落盘了）。
        if matches!(self.state, State::Submitting) || std::mem::discriminant(&self.state) != before
        {
            ctx.request_repaint();
        }
    }

    // eframe 0.36 起入口是 `ui`（不再是 `update`）；拿 Context 用 `ui.ctx()`。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(2.0);
            ui.heading("AICRM 登录");
            ui.add_space(12.0);

            let enabled = !self.busy();
            let mut submit = false;

            egui::Grid::new("credentials")
                .num_columns(2)
                .spacing([10.0, 10.0])
                .show(ui, |ui| {
                    ui.label("账号");
                    ui.add_enabled(
                        enabled,
                        egui::TextEdit::singleline(&mut self.username).desired_width(200.0),
                    );
                    ui.end_row();

                    ui.label("密码");
                    let field = ui.add_enabled(
                        enabled,
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .desired_width(200.0),
                    );
                    // 回车即提交
                    submit |=
                        field.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                    ui.end_row();
                });

            // 回车即提交，不要求焦点正好停在密码框上。
            if enabled && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                submit = true;
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::new("登 录").min_size([96.0, 26.0].into()),
                    )
                    .clicked()
                {
                    submit = true;
                }
                if ui
                    .add(egui::Button::new("退 出").min_size([72.0, 26.0].into()))
                    .clicked()
                {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });

            ui.add_space(10.0);
            self.status_ui(ui);

            // 明文传输密码是这套方案最危险的失误，因此常驻提示。
            if let Some(warning) = cloud::transport_warning(&self.config.endpoint) {
                ui.add_space(4.0);
                ui.colored_label(egui::Color32::from_rgb(190, 50, 50), warning);
            }

            if submit {
                let ctx = ui.ctx().clone();
                self.submit(&ctx);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::mpsc::{Receiver, channel},
    };

    /// 起一个只服务一次请求的假云端，返回它的 URL 和"收到的原始请求"的接收端。
    fn mock_cloud(status_line: &'static str, body: &'static str) -> (String, Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = channel();

        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();

            // 必须按 Content-Length 读满，单次 read 可能只拿到请求头。
            let mut raw = Vec::new();
            let mut chunk = [0u8; 1024];
            loop {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                raw.extend_from_slice(&chunk[..read]);
                let text = String::from_utf8_lossy(&raw);
                if let Some(head) = text.find("\r\n\r\n") {
                    // 头部名大小写不敏感：ureq 发的是小写 content-length。
                    let mut length = 0usize;
                    for line in text[..head].lines() {
                        if let Some((name, value)) = line.split_once(':')
                            && name.eq_ignore_ascii_case("content-length")
                        {
                            length = value.trim().parse().unwrap_or(0);
                        }
                    }
                    if raw.len() >= head + 4 + length {
                        break;
                    }
                }
            }
            let _ = sender.send(String::from_utf8_lossy(&raw).into_owned());

            let response = format!(
                "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        });

        (format!("http://{address}/api/auth/login"), receiver)
    }

    #[test]
    fn sends_credentials_as_json_and_reads_the_session() {
        // 真实云端（TangHang POST /api/auth/login）的响应形状
        let (endpoint, received) = mock_cloud(
            "200 OK",
            r#"{"code":0,"message":"success","data":{
                "accessToken":"eyJhbGciOi.access","refreshToken":"eyJhbGciOi.refresh",
                "user":{"user":{"name":"系统管理员","username":"admin"},"roles":[
                    {"name":"admin","display_name":"管理员"}]}}}"#,
        );

        let session = cloud::login(&endpoint, "zhangsan", "hunter2", "device-1").unwrap();

        assert_eq!(session.access_token, "eyJhbGciOi.access");
        assert_eq!(session.refresh_token.as_deref(), Some("eyJhbGciOi.refresh"));
        assert_eq!(session.display_name.as_deref(), Some("系统管理员"));

        let request = received.recv().unwrap();
        assert!(
            request.starts_with("POST /api/auth/login HTTP/1.1"),
            "{request}"
        );
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json"),
            "{request}"
        );
        // 关键：这个请求绝不能带 Authorization —— 它就是用来换取令牌的入口。
        assert!(
            !request.to_ascii_lowercase().contains("authorization:"),
            "登录请求不应携带 Authorization 头：{request}"
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap();
        let payload: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(payload["username"], "zhangsan");
        assert_eq!(payload["password"], "hunter2");
        assert_eq!(payload["device_id"], "device-1");
        assert!(payload["client_version"].is_string());
    }

    #[test]
    fn falls_back_to_username_when_display_name_is_absent() {
        let (endpoint, _) = mock_cloud(
            "200 OK",
            r#"{"code":0,"message":"success","data":{"accessToken":"tok","user":{"user":{"username":"admin"}}}}"#,
        );
        let session = cloud::login(&endpoint, "admin", "x", "device-1").unwrap();
        assert_eq!(session.display_name.as_deref(), Some("admin"));
        assert!(session.refresh_token.is_none());
    }

    #[test]
    fn rejected_credentials_surface_the_cloud_message() {
        // 真实云端的错误信封：{code, message, data:null}
        let (endpoint, _) = mock_cloud(
            "401 Unauthorized",
            r#"{"code":401,"message":"账号或密码错误","data":null}"#,
        );

        let error = cloud::login(&endpoint, "zhangsan", "wrong", "device-1").unwrap_err();
        assert!(matches!(error, LoginError::Rejected(_)), "{error:?}");
        assert_eq!(error.message(), "账号或密码错误");
    }

    #[test]
    fn success_envelope_without_data_is_a_protocol_error() {
        let (endpoint, _) = mock_cloud("200 OK", r#"{"code":0,"message":"success","data":null}"#);
        let error = cloud::login(&endpoint, "admin", "x", "device-1").unwrap_err();
        assert!(matches!(error, LoginError::Protocol(_)), "{error:?}");
    }

    #[test]
    fn server_fault_is_retryable_not_a_credential_problem() {
        let (endpoint, _) = mock_cloud("500 Internal Server Error", r#"{"message":"boom"}"#);

        let error = cloud::login(&endpoint, "zhangsan", "hunter2", "device-1").unwrap_err();
        assert!(matches!(error, LoginError::Unreachable(_)), "{error:?}");
    }

    #[test]
    fn non_contract_response_is_reported_as_protocol_error() {
        let (endpoint, _) = mock_cloud("200 OK", r#"{"unexpected":true}"#);

        let error = cloud::login(&endpoint, "zhangsan", "hunter2", "device-1").unwrap_err();
        assert!(matches!(error, LoginError::Protocol(_)), "{error:?}");
    }

    #[test]
    fn plain_http_to_a_remote_host_is_flagged() {
        assert!(cloud::transport_warning("https://api.example.com/api/login").is_none());
        // 本机联调不算问题
        assert!(cloud::transport_warning("http://127.0.0.1:9000/api/login").is_none());
        assert!(cloud::transport_warning("http://localhost:9000/api/login").is_none());
        // 明文发到公网必须报警
        assert!(cloud::transport_warning("http://api.example.com/api/login").is_some());
    }

    #[test]
    fn device_id_is_stable_across_calls() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("device.id");
        let first = store::device_id(&path);
        assert_eq!(first.len(), 32);
        assert_eq!(first, store::device_id(&path));
    }
}
