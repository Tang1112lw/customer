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
    // 云端命令通道：三项都齐了才启用轮询，缺任一就只跑本地 API。
    let mut cloud_url = None;
    let mut cloud_url_file = None;
    let mut device_token = None;
    let mut device_token_file = None;
    let mut device_id = None;
    // 允许调用本客户端接口的网页来源，可重复。部署到客户机器时用来放行云端域名。
    let mut allow_origins: Vec<String> = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(flag) = args.next() {
        if flag == "--help" {
            println!(
                "AICRM Windows API:\n  \
                 --tools-dir DIR --key-dir DIR --data-dir DIR --token-file FILE --bind IP:PORT\n  \
                 命令通道（可选，三项齐全才启用）：\n  \
                 --cloud-url URL            云端基地址，如 https://api.example.com\n  \
                 --cloud-url-file FILE      从文件读云端基地址。隧道地址会变时用这个：\n  \
                                            改文件即可，不用重装（每轮轮询都会重读）\n  \
                 --device-token TOK         设备令牌（明文写在命令行，仅调试用）\n  \
                 --device-token-file FILE   设备令牌文件（推荐：不会出现在命令行和自启项里）\n  \
                 --device-id ID             设备标识"
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
            "--device-token" => device_token = Some(value),
            "--device-token-file" => device_token_file = Some(PathBuf::from(value)),
            "--device-id" => device_id = Some(value),
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

    // 参数解析完毕，data 目录已确定，这时才能找「已就绪」的更新。
    // 文件优先于命令行：命令行里的令牌会出现在进程列表和 HKCU Run 自启项中。
    let device_token = match (device_token_file, device_token) {
        (Some(path), _) => Some(std::fs::read_to_string(&path).map_err(|error| {
            io::Error::other(format!("读取设备令牌文件 {} 失败：{error}", path.display()))
        })?),
        (None, inline) => inline,
    }
    .map(|token| token.trim().to_owned())
    .filter(|token| !token.is_empty());

    backend::set_allowed_origins(allow_origins);
    // 客户端连哪个云端，就自动信任来自该云端地址的网页 —— 不用为部署地址改代码。
    backend::set_cloud_origin(cloud_url.clone(), cloud_url_file.clone());

    // 令牌文件路径后面还要给设备认领用（读登录令牌），这里保留一份。
    let service = Service::new(tools, keys, data, token_file.clone())?;

    // 云端地址可以来自命令行，也可以来自文件 —— 隧道地址会变时用文件更方便：
    // 改文件即可，不用重装客户端。两者至少要有一个。
    if let (Some(device_token), Some(device_id)) = (device_token, device_id)
        && (cloud_url.is_some() || cloud_url_file.is_some())
    {
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
                // 设备归属跟着登录账号走：换账号登录后重启客户端，设备自动改挂过去。
                user_token_file: Some(token_file.clone()),
                device_token,
                device_id,
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
