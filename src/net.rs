//! HTTP layer: shared client, checked fetches and a concurrent downloader with
//! sha1 verification.

use crate::progress::ProgressSender;
use crate::util;
use anyhow::{anyhow, bail, Context, Result};
use futures::stream::{self, StreamExt};
use serde::de::DeserializeOwned;
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const USER_AGENT: &str = concat!(
    "NewBeeLauncher/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/newbee-launcher)"
);

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(90))
        .connect_timeout(Duration::from_secs(20))
        .pool_max_idle_per_host(32)
        .build()
        .unwrap_or_default()
}

pub async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3u32 {
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp.text().await?);
                }
                last = Some(anyhow!("HTTP {status} for {url}"));
                if status.as_u16() < 500 {
                    break;
                }
            }
            Err(e) => last = Some(e.into()),
        }
        tokio::time::sleep(Duration::from_millis(400 * u64::from(attempt + 1))).await;
    }
    Err(last.unwrap_or_else(|| anyhow!("request to {url} failed")))
}

pub async fn get_json<T: DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<T> {
    let text = get_text(client, url).await?;
    serde_json::from_str(&text).with_context(|| format!("parsing JSON from {url}"))
}

pub async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3u32 {
        match client.get(url).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    return Ok(resp.bytes().await?.to_vec());
                }
                last = Some(anyhow!("HTTP {status} for {url}"));
                if status.as_u16() < 500 {
                    break;
                }
            }
            Err(e) => last = Some(e.into()),
        }
        tokio::time::sleep(Duration::from_millis(400 * u64::from(attempt + 1))).await;
    }
    Err(last.unwrap_or_else(|| anyhow!("request to {url} failed")))
}

/// One file to fetch.
#[derive(Clone, Debug)]
pub struct DlTask {
    pub url: String,
    pub dest: PathBuf,
    pub sha1: Option<String>,
    pub size: Option<u64>,
    /// Human readable name shown in logs.
    pub label: String,
    /// Skip if the file already exists (used for jars we cannot hash).
    pub skip_if_exists: bool,
}

impl DlTask {
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>, sha1: Option<String>) -> Self {
        let url = url.into();
        let dest = dest.into();
        let label = dest
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| url.clone());
        Self {
            url,
            dest,
            sha1,
            size: None,
            label,
            skip_if_exists: false,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn sha1(mut self, sha1: impl Into<String>) -> Self {
        self.sha1 = Some(sha1.into());
        self
    }

    pub fn size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }
}

async fn try_download(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: Option<&str>,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = resp.status();
    if !status.is_success() {
        bail!("HTTP {status} for {url}");
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let file_name = dest
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "download".into());
    let tmp = dest.with_file_name(format!("{file_name}.part"));
    let mut file = tokio::fs::File::create(&tmp)
        .await
        .with_context(|| format!("creating {}", tmp.display()))?;
    let mut hasher = Sha1::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("body of {url}"))?;
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    drop(file);

    let got = hex::encode(hasher.finalize());
    if let Some(exp) = expected {
        if !got.eq_ignore_ascii_case(exp) {
            let _ = tokio::fs::remove_file(&tmp).await;
            bail!("sha1 mismatch for {url}: got {got}, expected {exp}");
        }
    }
    // Windows: rename over an existing file needs the target gone first, and a
    // running JVM may still hold the old jar open — retry briefly before giving up.
    if dest.exists() {
        let _ = tokio::fs::remove_file(dest).await;
    }
    if tokio::fs::rename(&tmp, dest).await.is_ok() {
        return Ok(());
    }
    tokio::time::sleep(Duration::from_millis(250)).await;
    let _ = tokio::fs::remove_file(dest).await;
    match tokio::fs::rename(&tmp, dest).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            bail!(
                "无法写入 {}（{e}）。如果这个实例正在运行，请先关闭游戏再重试。",
                dest.display()
            )
        }
    }
}

/// Download a single file, skipping it when the hash already matches.
pub async fn download_to(
    client: &reqwest::Client,
    url: &str,
    dest: &Path,
    expected: Option<&str>,
    skip_if_exists: bool,
) -> Result<()> {
    if dest.exists() {
        if let Some(exp) = expected {
            if util::sha1_file(dest).map(|h| h.eq_ignore_ascii_case(exp)).unwrap_or(false) {
                return Ok(());
            }
        } else if skip_if_exists {
            return Ok(());
        }
    }
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3u32 {
        match try_download(client, url, dest, expected).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt + 1))).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("download of {url} failed")))
}

/// Download many files concurrently. Remaining failures are collected and reported
/// once so a single flaky CDN object does not hide the rest of the work.
pub async fn download_many(
    client: &reqwest::Client,
    tasks: Vec<DlTask>,
    concurrency: usize,
    tx: &ProgressSender,
    stage: &str,
) -> Result<()> {
    if tasks.is_empty() {
        return Ok(());
    }
    let total = tasks.len();
    tx.stage(format!("{stage} · {total} 个文件"));
    let finished = Arc::new(AtomicUsize::new(0));
    let failures = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let stream = stream::iter(tasks.into_iter().map(|t| {
        let client = client.clone();
        let tx = tx.clone();
        let finished = finished.clone();
        let failures = failures.clone();
        async move {
            let res = download_to(
                &client,
                &t.url,
                &t.dest,
                t.sha1.as_deref(),
                t.skip_if_exists,
            )
            .await;
            if let Err(e) = &res {
                failures
                    .lock()
                    .map(|mut v| v.push(format!("{}: {e}", t.label)))
                    .ok();
            }
            let n = finished.fetch_add(1, Ordering::Relaxed) + 1;
            tx.progress(stage.to_string(), n as u64, total as u64);
            res
        }
    }))
    .buffer_unordered(concurrency.max(1));

    let results: Vec<Result<()>> = stream.collect().await;
    let _ = results;
    let errs = failures.lock().map(|v| v.clone()).unwrap_or_default();
    if !errs.is_empty() {
        let shown = errs.iter().take(5).cloned().collect::<Vec<_>>().join("; ");
        bail!("{} 个文件下载失败（{}）", errs.len(), shown);
    }
    Ok(())
}