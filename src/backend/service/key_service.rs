//! `AICRM-Key.exe` 业务：密钥准备与本地账号查询。
//!
//! 只涉及密钥辅助程序；导出相关逻辑见 [`super::export_service`]。

use super::{ApiError, ApiResult, Service, option};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fs, time::Duration};

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
}
