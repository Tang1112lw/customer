mod backend;

use backend::{
    app,
    service::{Service, agent_service},
};
use std::{env, io, path::PathBuf, time::Duration};

/// 两次长轮询之间的小憩。真正的节奏由云端的 `waitSeconds` 决定（服务端会挂起请求），
/// 这里只用来在云端连续报错时避免打爆它。
const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = env::current_exe()?.parent().unwrap().to_path_buf();
    let mut tools = root.join("tools");
    let mut keys = root.join("accounts");
    let mut data = root.join("data");
    let mut token_file = root.join("api.token");
    let mut bind = "127.0.0.1:8765".to_owned();
    // 云端命令通道：**只认登录**（api.token 是云端会话时才接入；见下方接入判断）。
    let mut cloud_url = None;
    let mut cloud_url_file = None;
    // 允许调用本客户端接口的网页来源，可重复。部署到客户机器时用来放行云端域名。
    let mut allow_origins: Vec<String> = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "AICRM Windows API（参数全部可选）：\n  \
                 缺省时一切从 exe 同目录读：tools/、accounts/、data/、api.token、cloud.url。\n  \
                 --tools-dir DIR --key-dir DIR --data-dir DIR --token-file FILE --bind IP:PORT\n  \
                 命令通道（登录后才启用；没有配对、没有设备身份）：\n  \
                 --cloud-url URL            云端基地址，如 https://api.example.com\n  \
                 --cloud-url-file FILE      从文件读云端基地址。隧道地址会变时用这个：\n  \
                                            改文件即可，不用重装（每轮轮询都会重读）\n  \
                 --device-token / --device-token-file / --device-id  已废弃：\n  \
                                            仅兼容旧版自启项，解析后忽略"
            );
            return Ok(());
        }
        let value = args
            .next()
            .ok_or_else(|| io::Error::other(format!("{flag} 缺少参数")))?;
        match flag.as_str() {
            "--tools-dir" => tools = PathBuf::from(value),
            "--key-dir" => keys = PathBuf::from(value),
            "--data-dir" => data = PathBuf::from(value),
            "--token-file" => token_file = PathBuf::from(value),
            "--bind" => bind = value,
            "--cloud-url" => cloud_url = Some(value),
            "--cloud-url-file" => cloud_url_file = Some(PathBuf::from(value)),
            // 设备绑定（配对 / 设备令牌）已于 2026-10-10 取消：这三个参数只为
            // 老版本装出来的自启项兼容而保留 —— 解析但不使用，缺了它们也不报错。
            "--device-token" | "--device-token-file" | "--device-id" => {}
            // 网页来源白名单，可重复。例：--allow-origin https://aicrm.example.com
            "--allow-origin" => allow_origins.push(value),
            _ => return Err(io::Error::other(format!("未知参数: {flag}")).into()),
        }
    }

    // 无条件打一行关键路径与待更新状态：更新出问题时，这一行能直接定位是
    // 「没下载到」「路径不对」还是「版本比较判成了不更新」。
    println!(
        "启动：程序目录 {}；数据目录 {}；待应用版本 {:?}；当前版本 {}",
        root.display(),
        data.display(),
        backend::service::pending_update_version(&root),
        env!("CARGO_PKG_VERSION")
    );

    // 启动时先看有没有「已就绪」的新版本：有就交给更新器替换并重启，
    // 然后本进程退出。微信就是这个模型 —— 更新在下次启动时生效，
    // 客户正用着的时候绝不会被打断（采集跑到一半被杀就是事故）。
    if let Some(version) = backend::service::pending_update_version(&root) {
        if !backend::service::newer_version(&version, env!("CARGO_PKG_VERSION")) {
            // 不比当前新（已经应用过，或服务端回退发布）→ 清掉标记，绝不当降级用
            let _ = std::fs::remove_file(root.join("updates").join("pending.json"));
        } else {
            backend::service::record_startup_command(&data);
            let updater = root.join("aicrm-update.exe");
            if updater.is_file()
                && backend::service::agent_service::launch_updater(&updater, &root).is_ok()
            {
                println!("发现已就绪的新版本 {version}，交给更新器应用后重启");
                return Ok(());
            }
        }
    }

    // 客户端实例标识：本地生成（UUID），只用于云端区分同一个账号的多台电脑 ——
    // 它就是「一个账号只能登录一个客户端」的判定依据。
    // **不是凭据**：不需要任何签发或配对，生成一次后不再变化。
    let client_id = read_trimmed(&root.join("client.id")).unwrap_or_else(|| {
        let generated = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::write(root.join("client.id"), &generated);
        generated
    });

    // 命令通道**只认登录**：api.token 是一个没过期的云端会话令牌（登录 / 续期写入）
    // 时才接入。安装时生成的本地随机令牌不算登录，过期的令牌也不算。
    let logged_in = std::fs::read_to_string(&token_file)
        .map(|text| backend::session::token_is_live(&text))
        .unwrap_or(false);

    backend::set_allowed_origins(allow_origins);
    // 云端地址：exe 旁的 `cloud.url` 存在时即为唯一事实来源（隧道地址会变，改文件
    // 即可、不用重装；每轮轮询都会重读）。参数仅在文件缺失时兜底。
    // 客户端连哪个云端，就自动信任来自该云端地址的网页 —— 不用为部署地址改代码。
    let default_cloud = root.join("cloud.url");
    let (cloud_url, cloud_url_file) = if default_cloud.is_file() {
        (None, Some(default_cloud))
    } else {
        (cloud_url, cloud_url_file)
    };
    backend::set_cloud_origin(cloud_url.clone(), cloud_url_file.clone());

    // 令牌文件路径还要交给命令通道（每次请求现读登录令牌），这里保留一份。
    let service = Service::new(tools, keys, data, token_file.clone())?;

    // 云端地址 + 已登录，两样齐了才启用命令通道；缺任一就只跑本地 API。
    if logged_in && (cloud_url.is_some() || cloud_url_file.is_some()) {
        match &cloud_url_file {
            Some(path) => eprintln!("命令通道已启用，云端地址读自 {}", path.display()),
            None => eprintln!(
                "命令通道已启用，轮询 {}",
                cloud_url.as_deref().unwrap_or_default()
            ),
        }
        agent_service::spawn(
            service.clone(),
            agent_service::CloudConfig {
                base_url: cloud_url.unwrap_or_default(),
                base_url_file: cloud_url_file,
                // 归属就是登录账号本身：换账号登录后重启客户端，网页端立刻看到新账号。
                user_token_file: Some(token_file.clone()),
                client_id,
                interval: POLL_INTERVAL,
            },
        );
    }

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    eprintln!("AICRM Windows API listening on {}", listener.local_addr()?);
    axum::serve(listener, app(service))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// 读一个"一行文本"配置文件；不存在或为空白返回 `None`。
///
/// 与服务共处的 `cloud.url` / `client.id` 都用它读 ——
/// 判定口径一致（去空白、空文件不算存在）。
fn read_trimmed(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
