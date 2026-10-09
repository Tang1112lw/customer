//! `AICRM-Export.exe` 业务：导出执行、结果打包与 NDJSON 输出。
//!
//! 只涉及导出辅助程序；密钥相关逻辑见 [`super::key_service`]。

use super::{ApiError, ApiResult, Service, option};
use axum::{
    body::{Body, Bytes},
    http::header,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

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
            .run(EXPORT_PROGRAM, args, Duration::from_secs(1800))
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

    pub async fn export_bundle(&self, request: ExportRequest, ai: bool) -> ApiResult<Response> {
        let (_, job) = self.run_export(request).await?;
        let archive = job.with_extension("zip");
        let work = job.clone();
        let packed = tokio::task::spawn_blocking(move || -> io::Result<()> {
            if ai {
                write_chat(&work)?;
            }
            write_zip(&work, &work.with_extension("zip"))
        })
        .await
        .map_err(|_| ApiError::io())?
        .map_err(|_| ApiError::io());
        if let Err(error) = packed {
            let _ = fs::remove_dir_all(&job);
            let _ = fs::remove_file(&archive);
            return Err(error);
        }
        send_file(
            archive.clone(),
            Cleanup {
                dir: job,
                path: archive,
                file: None,
            },
            "application/zip",
            "aicrm-export.zip",
        )
        .await
    }

    pub async fn export_stream(&self, request: ExportRequest) -> ApiResult<Response> {
        let (result, job) = self.run_export(request).await?;
        let ndjson = job.with_extension("ndjson");
        let work = job.clone();
        let output = ndjson.clone();
        let built = tokio::task::spawn_blocking(move || write_ndjson(&work, &output, &result))
            .await
            .map_err(|_| ApiError::io())?
            .map_err(|_| ApiError::io());
        if let Err(error) = built {
            let _ = fs::remove_dir_all(&job);
            let _ = fs::remove_file(&ndjson);
            return Err(error);
        }
        send_file(
            ndjson.clone(),
            Cleanup {
                dir: job,
                path: ndjson,
                file: None,
            },
            "application/x-ndjson",
            "aicrm-export.ndjson",
        )
        .await
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

struct Cleanup {
    dir: PathBuf,
    path: PathBuf,
    file: Option<tokio::fs::File>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        self.file.take();
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir_all(&self.dir);
    }
}

async fn send_file(
    path: PathBuf,
    mut cleanup: Cleanup,
    kind: &'static str,
    name: &'static str,
) -> ApiResult<Response> {
    let size = fs::metadata(&path).map_err(|_| ApiError::io())?.len();
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|_| ApiError::io())?;
    cleanup.path = path;
    cleanup.file = Some(file);
    let stream = async_stream::stream! {
        let mut cleanup = cleanup;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            match cleanup.file.as_mut().unwrap().read(&mut buffer).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<Bytes, io::Error>(Bytes::copy_from_slice(&buffer[..n])),
                Err(error) => { yield Err(error); break; }
            }
        }
    };
    Ok((
        [
            (header::CONTENT_TYPE, kind),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{name}\""),
            ),
            (header::CONTENT_LENGTH, &size.to_string()),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

fn message_files(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if entry.file_name() == "messages.json" {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}

fn write_chat(root: &Path) -> io::Result<()> {
    let mut output = File::create(root.join("chat.txt"))?;
    for path in message_files(root)? {
        let value: Value = serde_json::from_reader(File::open(&path)?)?;
        let date = path
            .parent()
            .and_then(Path::file_name)
            .unwrap()
            .to_string_lossy();
        let chat = &value["chat"];
        let name = chat["display_name"].as_str().unwrap_or("未知会话");
        let id = chat["id"].as_str().unwrap_or("");
        writeln!(output, "\n# {date}｜{name}（{id}）")?;
        if let Some(messages) = value["messages"].as_array() {
            for message in messages {
                let speaker = if message["sender"]["is_self"] == true {
                    "我".to_owned()
                } else if chat["type"] == "group" {
                    let member = message["sender"]["display_name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .or_else(|| message["sender"]["id"].as_str())
                        .unwrap_or("");
                    if member.is_empty() {
                        "群成员".to_owned()
                    } else {
                        format!("群成员({})", member.replace(['\r', '\n'], " "))
                    }
                } else {
                    "对方".to_owned()
                };
                let content = &message["content"];
                let body = if content["type"] == "voice" {
                    let media = message["media"].as_str().unwrap_or("");
                    let relative = Path::new(media);
                    if relative
                        .components()
                        .all(|c| matches!(c, Component::Normal(_)))
                        && !media.is_empty()
                    {
                        let dir = path.parent().unwrap().strip_prefix(root).unwrap();
                        format!(
                            "[语音:{}]",
                            dir.join(relative).to_string_lossy().replace('\\', "/")
                        )
                    } else {
                        "[语音]".to_owned()
                    }
                } else {
                    content["text"]
                        .as_str()
                        .unwrap_or("")
                        .replace('\r', "\\r")
                        .replace('\n', "\\n")
                };
                writeln!(output, "{speaker}：{body}")?;
            }
        }
    }
    Ok(())
}

fn write_zip(root: &Path, archive: &Path) -> io::Result<()> {
    let mut zip = ZipWriter::new(File::create(archive)?);
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file() {
                let name = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let method = if name.ends_with(".json") || name.ends_with(".txt") {
                    CompressionMethod::Deflated
                } else {
                    CompressionMethod::Stored
                };
                zip.start_file(
                    name,
                    SimpleFileOptions::default().compression_method(method),
                )?;
                io::copy(&mut File::open(entry.path())?, &mut zip)?;
            }
        }
    }
    zip.finish()?;
    Ok(())
}

fn write_ndjson(root: &Path, path: &Path, summary: &Value) -> io::Result<()> {
    let mut output = File::create(path)?;
    serde_json::to_writer(
        &mut output,
        &json!({"type":"meta","schema":"aicrm-windows-files/v1"}),
    )?;
    writeln!(output)?;
    for file in message_files(root)? {
        let document: Value = serde_json::from_reader(File::open(&file)?)?;
        let relative = file
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        serde_json::to_writer(
            &mut output,
            &json!({"type":"file","path":relative,"document":document}),
        )?;
        writeln!(output)?;
    }
    serde_json::to_writer(
        &mut output,
        &json!({
            "type":"summary", "success":true,
            "source_wxid":summary["source_wxid"], "task_id":summary["task_id"],
            "conversation_count":summary["conversation_count"],
            "message_count":summary["message_count"], "media_count":summary["media_count"]
        }),
    )?;
    writeln!(output)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn ai_bundle_keeps_raw_voice_and_shortens_reading_text() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("job");
        let day = root.join("wxid_test").join("Alice").join("2026-09-29");
        fs::create_dir_all(&day).unwrap();
        let original = json!({
            "chat":{"display_name":"Alice","id":"wxid_alice","type":"private"},
            "messages":[
                {"sender":{"is_self":true},"content":{"type":"text","text":"hello"},"media":null},
                {"sender":{"is_self":false},"content":{"type":"voice","text":"<msg><voicemsg aeskey=\"secret\"/></msg>"},"media":"audio/1.silk"}
            ]
        });
        let raw = serde_json::to_vec_pretty(&original).unwrap();
        fs::write(day.join("messages.json"), &raw).unwrap();
        fs::create_dir(day.join("audio")).unwrap();
        fs::write(day.join("audio/1.silk"), b"voice").unwrap();

        write_chat(&root).unwrap();
        let chat = fs::read_to_string(root.join("chat.txt")).unwrap();
        assert!(chat.contains("我：hello"));
        assert!(chat.contains("对方：[语音:wxid_test/Alice/2026-09-29/audio/1.silk]"));
        assert!(!chat.contains("voicemsg"));
        assert_eq!(fs::read(day.join("messages.json")).unwrap(), raw);

        let archive = temp.path().join("job.zip");
        write_zip(&root, &archive).unwrap();
        let mut zip = zip::ZipArchive::new(File::open(archive).unwrap()).unwrap();
        let mut zipped_raw = Vec::new();
        zip.by_name("wxid_test/Alice/2026-09-29/messages.json")
            .unwrap()
            .read_to_end(&mut zipped_raw)
            .unwrap();
        assert_eq!(zipped_raw, raw);
        assert!(
            zip.by_name("wxid_test/Alice/2026-09-29/audio/1.silk")
                .is_ok()
        );
    }

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
