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

impl Service {
    pub async fn key_version(&self) -> ApiResult<Value> {
        self.version(KEY_PROGRAM).await
    }

    pub async fn prepare(&self, request: PrepareRequest) -> ApiResult<Value> {
        // 本机已经有**对得上**的密钥与解密缓存时，直接成功返回，不再去抓一遍。
        //
        // 抓密钥会重启微信、还要客户配合重新登录 —— 客户正用着电脑的时候，这是最
        // 容易被投诉的动作；而且一旦抓取失败，整条绑定就卡在"正在检测本机微信…"
        // 再也不回来（实测卡死过）。密钥本来就在、且就是要绑的那个号，复用就是了。
        //
        // 但「复用」必须克制，只在两种情况下发生：
        //   1. 核对模式（no_restart）—— 只看不抓，返回本机已有结论；
        //   2. 定向重新绑定（expected 明确且与本机缓存一致）—— 明确就是这个号。
        // 其余一律真抓。**换过微信账号之后再点绑定**（expected 为空）必须真抓当前
        // 微信，否则会把旧账号的密钥「复用成功」—— 实测踩过：客户换号后点绑定
        // 秒回旧微信号，新号的密钥根本没抓，云端还把新账号记到了旧微信号上。
        if !request.force
            && let Some((wxid, credential, cache)) = self.key_state()
            && credential
            && cache
            && (request.no_restart
                || request
                    .expected_wxid
                    .as_deref()
                    .is_some_and(|expected| !expected.is_empty() && expected == wxid))
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
        option(&mut args, "--weixin-exe", request.weixin_exe)?;
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
        let mut result = self
            .run(KEY_PROGRAM, args, Duration::from_secs(600))
            .await?;
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
