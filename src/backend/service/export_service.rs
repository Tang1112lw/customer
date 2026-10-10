//! Markdown 导出业务：调用 `aicrm-export-new.exe` 执行导出。
//!
//! 只涉及导出辅助程序；密钥相关逻辑见 [`super::key_service`]，
//! 归档打包与上传见 [`super::archive_service`]。
//!
//! 曾经还有「下载原始数据」的两个接口（NDJSON 流与 ZIP 打包，含 AI ZIP）——
//! 2026-10 审计确认前端与云命令通道都不再调用它们（采集闭环走
//! `wechat.collect` → 归档上传），整体删除。需要这种能力时从 git 历史取回。

use super::{ApiError, ApiResult, Service, option};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// 导出辅助程序名。`Service::new` 用它检查程序是否就位。
pub(crate) const EXPORT_PROGRAM: &str = "aicrm-export-new.exe";

static NEXT_JOB: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportRequest {
    pub wxid: Option<String>,
    pub person: Option<String>,
    pub day: Option<u32>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub task_id: Option<String>,
    pub wechat_dir: Option<String>,
    /// 云端指定的落盘目录。给了就直接导到这里（不清理、不删），
    /// 不给才用 `data/jobs/<id>` 临时目录。
    pub export_dir: Option<String>,
}

impl Service {
    pub async fn export_version(&self) -> ApiResult<Value> {
        self.version(EXPORT_PROGRAM).await
    }

    async fn run_export(&self, request: ExportRequest) -> ApiResult<(Value, PathBuf)> {
        validate_export(&request)?;
        let _lock = self.gate.read().await;
        // 云端可以指定落盘目录；不指定才用 data/jobs 下的临时任务目录。
        let job = match request.export_dir.as_deref() {
            Some(dir) => PathBuf::from(dir),
            None => self.data.join("jobs").join(job_id()),
        };
        fs::create_dir_all(&job).map_err(|_| ApiError::io())?;
        let mut args = vec![
            "export".into(),
            "--key-dir".into(),
            self.keys.display().to_string(),
            "--export-dir".into(),
            job.display().to_string(),
            "--json".into(),
        ];
        option(&mut args, "--wxid", request.wxid)?;
        option(&mut args, "--person", request.person)?;
        option(&mut args, "--start", request.start)?;
        option(&mut args, "--end", request.end)?;
        option(&mut args, "--task-id", request.task_id)?;
        option(&mut args, "--wechat-dir", request.wechat_dir)?;
        if let Some(day) = request.day {
            args.extend(["--day".into(), day.to_string()]);
        }
        let result = self
            .run(EXPORT_PROGRAM, args, Duration::from_secs(720))
            .await;
        // 只清理临时任务目录。云端指定的目录不能因为一次失败就删掉 ——
        // 那可能是用户/云端正在用的真实数据目录。
        if result.is_err() && request.export_dir.is_none() {
            let _ = fs::remove_dir_all(&job);
        }
        result.map(|v| (v, job))
    }

    pub async fn export_files(&self, request: ExportRequest) -> ApiResult<Value> {
        let (mut result, job) = self.run_export(request).await?;
        if let Some(object) = result.as_object_mut() {
            object.insert("job_dir".into(), json!(job));
        }
        Ok(result)
    }
}

fn validate_export(request: &ExportRequest) -> ApiResult<()> {
    if let Some(day) = request.day {
        if day == 0 || day > 36500 || request.start.is_some() || request.end.is_some() {
            return Err(ApiError::bad("day 应为 1-36500，且不能与 start/end 同用"));
        }
    }
    if request.start.is_some() != request.end.is_some() {
        return Err(ApiError::bad("start/end 必须同时提供"));
    }
    for date in [&request.start, &request.end].into_iter().flatten() {
        if date.len() != 10
            || !date.bytes().enumerate().all(|(i, b)| {
                if i == 4 || i == 7 {
                    b == b'-'
                } else {
                    b.is_ascii_digit()
                }
            })
        {
            return Err(ApiError::bad("日期格式须为 YYYY-MM-DD"));
        }
    }
    Ok(())
}

fn job_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{nanos}-{}", NEXT_JOB.fetch_add(1, Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_filter_is_unambiguous() {
        let request = ExportRequest {
            wxid: None,
            person: None,
            day: Some(1),
            start: Some("2026-09-29".into()),
            end: Some("2026-09-29".into()),
            task_id: None,
            wechat_dir: None,
            export_dir: None,
        };
        assert!(validate_export(&request).is_err());
    }
}
