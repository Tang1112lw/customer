//! `AICRM-Key.exe` 业务：密钥准备与本地账号查询。
//!
//! 只涉及密钥辅助程序；导出相关逻辑见 [`super::export_service`]。

use super::{ApiError, ApiResult, Service, option};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

/// 密钥辅助程序名。`Service::new` 用它检查程序是否就位。
pub(crate) const KEY_PROGRAM: &str = "AICRM-Key.exe";

/// 取密钥请求。
///
/// 同时接受 snake_case 与云端控制面使用的 camelCase：控制面下发 `wechat.bind` 命令时
/// 用的是 `captureTimeoutSeconds` / `expectedWxid` 这套命名。两套都要认，否则命令会在
/// 解析阶段就被 `deny_unknown_fields` 拒掉。
#[derive(Clone, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct PrepareRequest {
    #[serde(alias = "expectedWxid")]
    pub expected_wxid: Option<String>,
    #[serde(alias = "wechatDir")]
    pub wechat_dir: Option<String>,
    #[serde(alias = "weixinExe")]
    pub weixin_exe: Option<String>,
    #[serde(alias = "captureTimeoutSeconds")]
    pub capture_timeout: Option<u32>,
    #[serde(default)]
    pub force: bool,
    #[serde(default, alias = "noRestart")]
    pub no_restart: bool,
}

/// 把"工具自己找不到微信"这类原话翻译成客户能照做的话。
///
/// 工具的原话是「请用 --weixin-exe 指定 Weixin.exe 完整路径」—— 客户没有路径可填，
/// 这句话对他没有任何意义。客户端已经替它找过一遍（注册表 + 常见安装目录），
/// 所以这里只需要说清"这台机器上没找到微信"以及下一步做什么。
fn translate_key_error(error: ApiError, weixin_exe: Option<&str>) -> ApiError {
    let message = error.message().to_owned();
    let locating_failed = message.contains("无法定位微信")
        || message.contains("Weixin.exe")
        || message.contains("weixin-exe");
    if !locating_failed {
        return error;
    }
    let hint = match weixin_exe {
        Some(path) => format!("客户端已定位到 {path}，但它不是可用的微信程序"),
        None => "客户端已查过注册表与常见安装目录，都没找到微信程序".to_owned(),
    };
    ApiError::helper(format!(
        "这台电脑上没找到微信程序：请确认微信已安装并已登录，然后重新绑定。（{hint}；原始信息：{message}）"
    ))
}

impl Service {
    pub async fn key_version(&self) -> ApiResult<Value> {
        self.version(KEY_PROGRAM).await
    }

    pub async fn prepare(&self, request: PrepareRequest) -> ApiResult<Value> {
        // **云端让绑定，就真抓一遍。**
        //
        // 这里原先还留了一条"定向重绑（expected 匹配本机缓存）就复用"的捷径，2026-10-10
        // 按产品要求删掉：网页上点「绑定微信账号」的语义就是"去这台电脑上抓一次密钥"，
        // 复用会把"点了没反应"变成常态（客户以为在抓、其实一行都没跑），换号之后更是
        // 会把旧号的密钥当新号报上去。
        //
        // 唯一保留复用的地方是**核对模式**（`no_restart`）：那是服务自己的定期自检，
        // 只回答"本机密钥还在不在、够不够用"，绝不能每 10 分钟重启一次客户的微信。
        // 核对模式只由本机调用，云端命令不会带它。
        if request.no_restart
            && !request.force
            && let Some((wxid, credential, cache)) = self.key_state()
            && credential
            && cache
        {
            let (wxid, name) = self.local_account_label(&wxid);
            self.remember_bound_account(&wxid, &name)?;
            return Ok(json!({
                "success": true,
                "wxid": wxid,
                "name": name,
                "platform": "windows",
                "key_ready": true,
                "database_ready": true,
                "reused": true,
                "message": "本机已有可用的密钥与解密缓存，直接复用",
            }));
        }
        let _lock = self.gate.write().await;
        let mut args = vec![
            "prepare".into(),
            "--key-dir".into(),
            self.keys.display().to_string(),
            "--json".into(),
        ];
        option(&mut args, "--expected-wxid", request.expected_wxid)?;
        option(&mut args, "--wechat-dir", request.wechat_dir)?;
        // 微信程序路径：云端没给就自己找 —— 客户机把微信装在 D 盘、或装在工具认不出的
        // 目录都很常见，找不到时工具只会甩一句"请用 --weixin-exe 指定"，客户照做不了。
        let weixin_exe = request
            .weixin_exe
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| crate::backend::wechat::find_weixin_exe().map(|path| path.display().to_string()));
        option(&mut args, "--weixin-exe", weixin_exe.clone())?;
        if let Some(n) = request.capture_timeout {
            if !(10..=300).contains(&n) {
                return Err(ApiError::bad("capture_timeout 须为 10-300"));
            }
            args.extend(["--capture-timeout".into(), n.to_string()]);
        }
        if request.force {
            args.push("--force".into());
        }
        if request.no_restart {
            args.push("--no-restart".into());
        }
        let mut result = match self.run(KEY_PROGRAM, args, Duration::from_secs(600)).await {
            Ok(value) => value,
            Err(error) => return Err(translate_key_error(error, weixin_exe.as_deref())),
        };
        if let Some(object) = result.as_object_mut() {
            object.remove("key_word");
        }
        // 真抓成功时顺手更新本机状态：界面卡片与后续判断都读 bound-wechat.json，
        // 不写的话换号之后卡片还显示旧账号（实测踩过）。
        if let Some(object) = result.as_object() {
            let wxid = object.get("wxid").and_then(Value::as_str).unwrap_or_default();
            if !wxid.is_empty() {
                let name = object
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(wxid)
                    .to_owned();
                let _ = self.remember_bound_account(wxid, &name);
            }
        }
        Ok(result)
    }

    pub fn accounts(&self) -> ApiResult<Value> {
        let mut accounts = Vec::new();
        for entry in fs::read_dir(&self.keys).map_err(|_| ApiError::io())? {
            let entry = entry.map_err(|_| ApiError::io())?;
            if entry.file_type().map_err(|_| ApiError::io())?.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("wxid_")
                    && entry.path().join("credentials.local.json").is_file()
                {
                    accounts.push(json!({"wxid":name,"key_ready":true}));
                }
            }
        }
        accounts.sort_by(|a, b| a["wxid"].as_str().cmp(&b["wxid"].as_str()));
        Ok(json!({"success":true,"accounts":accounts}))
    }

    /// 扫描本机密钥现状：`(wxid, 有密钥本体, 有解密缓存)`。
    ///
    /// 「密钥能用」得两样都在：`credentials.local.json` 是抓到的密钥本体，
    /// 解密出来的聊天库在 **`<key-dir>/cache/`**（见账号目录里的 config.json：
    /// `"decrypted_dir": "cache"`）。只有密钥、没有缓存时导出读不到库
    /// （实测会一直空等到云端 15 分钟超时），但界面和云端都容易误判成就绪 ——
    /// 所以自检必须把这两种情况分开看。
    pub(crate) fn key_state(&self) -> Option<(String, bool, bool)> {
        // 换过微信账号的机器，密钥目录下会同时躺着旧号的目录 —— 必须取
        // 「最近一次抓到的那一个」（当前登录的号），按枚举顺序拿第一个
        // 会一直指着旧号（实测踩过：换号重绑成功后仍报旧号）。
        let name = super::newest_account_dir(&self.keys)?;
        let path = self.keys.join(&name);
        let credential = path.join("credentials.local.json").is_file();
        let cache = super::decrypted_cache_ready(&self.keys, &name);
        Some((name, credential, cache))
    }

    /// 启动自检并核对本机密钥：调用 `AICRM-Key.exe prepare --no-restart`。
    ///
    /// 用途（客户的诉求）：**更新之后自动核对一次** —— 密钥在就显示「已绑定 微信号 xxx」，
    /// 云端照常按心跳里的就绪状态下发任务。用 `--no-restart` 是刻意的：
    /// 只核对、不重启微信；重抓密钥是客户自己点「绑定」时才做的动作。
    ///
    /// 返回 `(wxid, 显示名)`；本机没有账号或核对没过时返回 None。
    pub(crate) async fn verify_local_key(&self) -> ApiResult<Option<(String, String)>> {
        let Some((wxid, credential, cache)) = self.key_state() else {
            return Ok(None);
        };
        if !credential || !cache {
            return Ok(None);
        }
        let label = self.local_account_label(&wxid);
        // 一分钟内不重复调用辅助程序；这段时间内直接按文件判定为就绪。
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or(0);
        let previous = LAST_LOCAL_VERIFY.load(Ordering::SeqCst);
        if previous == 0 || now.saturating_sub(previous) >= LOCAL_VERIFY_COOLDOWN {
            LAST_LOCAL_VERIFY.store(now, Ordering::SeqCst);
            let request = PrepareRequest {
                expected_wxid: Some(wxid.clone()),
                wechat_dir: None,
                weixin_exe: None,
                capture_timeout: None,
                force: false,
                no_restart: true,
            };
            self.prepare(request).await?;
        }
        self.remember_bound_account(&label.0, &label.1)?;
        Ok(Some(label))
    }

    /// 从账号目录里的 `config.json` 取显示名（微信昵称），取不到就回落成 wxid。
    fn local_account_label(&self, wxid: &str) -> (String, String) {
        let raw = fs::read_to_string(self.keys.join(wxid).join("config.json"));
        let name = raw
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| {
                value
                    .get("nickname")
                    .or_else(|| value.get("wechat_id"))
                    .and_then(|item| item.as_str())
                    .map(str::to_owned)
            })
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| wxid.to_owned());
        (wxid.to_owned(), name)
    }

    /// 把「本机已绑定哪个微信号」写进 `<data>/state/bound-wechat.json`，界面直接读它。
    ///
    /// `source=local` 表示这是**本机密钥核对**得出的结论，而不是云端绑定的记录 ——
    /// 界面据此提示"云端尚未登记，可在网页点绑定"。
    fn remember_bound_account(&self, wxid: &str, name: &str) -> ApiResult<()> {
        let directory = self.data.join("state");
        fs::create_dir_all(&directory).map_err(|_| ApiError::io())?;
        let payload = json!({
            "wxid": wxid,
            "name": name,
            "source": "local",
            "at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|value| value.as_secs())
                .unwrap_or(0),
        });
        fs::write(
            directory.join("bound-wechat.json"),
            serde_json::to_vec_pretty(&payload).unwrap_or_default(),
        )
        .map_err(|_| ApiError::io())
    }

    /// 启动自检：扫描本机是否绑定了微信、密钥与解密缓存是否齐全。
    ///
    /// **只看不动**。缺什么就如实报什么，由云端/界面显示成「未绑定」，
    /// 等客户在微信登录状态下自己点「重新绑定」补回来 —— 抓密钥会重启微信，
    /// 是客户能感知到的动作，不能由客户端在后台自作主张地干。
    ///
    /// 返回一句人话，供启动日志与排查用。
    pub(crate) fn key_state_summary(&self) -> String {
        match self.key_state() {
            None => "尚未绑定微信（本机没有密钥目录）".to_owned(),
            Some((wxid, true, true)) => format!("{wxid} 密钥与解密缓存齐全"),
            Some((wxid, true, false)) => {
                format!("{wxid} 有密钥但缺解密缓存，网页端按未绑定处理，需重新绑定")
            }
            Some((wxid, false, _)) => {
                format!("{wxid} 缺密钥，网页端按未绑定处理，需重新绑定")
            }
        }
    }
}

/// 上一次自动核对密钥的时刻（Unix 秒）。0 = 本次启动还没核对过。
static LAST_LOCAL_VERIFY: AtomicU64 = AtomicU64::new(0);

/// 自动核对的最小间隔：一次核对会调用辅助程序，不能每轮都来。
const LOCAL_VERIFY_COOLDOWN: u64 = 60;
