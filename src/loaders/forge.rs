//! Forge and NeoForge.
//!
//! Modern installers are headless-capable (`--installClient <dir>`) and do all
//! of the deobfuscation/patcher work themselves, so we download the installer,
//! run it with a Java 17+ runtime and then discover which version id appeared.

use super::LoaderChoice;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;

const FORGE_MAVEN: &str = "https://maven.minecraftforge.net/net/minecraftforge/forge";
const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases/net/neoforged/neoforge";

/// Extract `<version>` entries from a maven-metadata.xml.
fn parse_maven_versions(xml: &str) -> Vec<String> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut out = Vec::new();
    let mut inside = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.name().as_ref().ends_with(b"version") {
                    inside = true;
                }
            }
            Ok(Event::Text(t)) if inside => {
                if let Ok(s) = t.unescape() {
                    let s = s.trim().to_string();
                    if !s.is_empty() {
                        out.push(s);
                    }
                }
                inside = false;
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref().ends_with(b"version") {
                    inside = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

async fn fetch_maven_versions(ctx: &Ctx, base: &str) -> Result<Vec<String>> {
    let url = format!("{base}/maven-metadata.xml");
    let xml = crate::net::get_text(&ctx.client, &url)
        .await
        .with_context(|| format!("获取 {url}"))?;
    let versions = parse_maven_versions(&xml);
    if versions.is_empty() {
        bail!("{url} 里没有版本");
    }
    Ok(versions)
}

fn version_key(v: &str) -> Vec<u32> {
    v.split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.parse::<u32>().ok())
        .collect()
}

pub async fn list_forge(ctx: &Ctx, mc: &str) -> Result<Vec<LoaderChoice>> {
    let all = fetch_maven_versions(ctx, FORGE_MAVEN).await?;
    let prefix = format!("{mc}-");
    let mut out: Vec<String> = all
        .into_iter()
        .filter(|v| v.starts_with(&prefix))
        .map(|v| v[prefix.len()..].to_string())
        .filter(|v| !v.is_empty())
        .collect();
    out.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    out.dedup();
    Ok(out
        .into_iter()
        .map(|version| {
            let stable = !version.contains("beta") && !version.contains("alpha");
            LoaderChoice { version, stable }
        })
        .collect())
}

/// NeoForge numbers releases as `<mc-minor>.<mc-patch>.<build>` (1.21.1 → 21.1.x).
fn neoforge_matches(mc: &str, v: &str) -> bool {
    let short = mc.strip_prefix("1.").unwrap_or(mc);
    v.starts_with(&format!("{short}.")) || v == short
}

pub async fn list_neoforge(ctx: &Ctx, mc: &str) -> Result<Vec<LoaderChoice>> {
    let all = fetch_maven_versions(ctx, NEOFORGE_MAVEN).await?;
    let mut out: Vec<String> = all
        .into_iter()
        .filter(|v| neoforge_matches(mc, v))
        .collect();
    out.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    out.dedup();
    Ok(out
        .into_iter()
        .map(|version| {
            let stable = !version.contains("beta") && !version.contains("alpha");
            LoaderChoice { version, stable }
        })
        .collect())
}

async fn pump<R>(reader: R, tx: crate::progress::ProgressSender)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tx.log(format!("  {line}"));
    }
}

/// Forge/NeoForge installers refuse to run unless a vanilla launcher profile
/// file exists in the target directory, so we plant a minimal one.
fn ensure_launcher_profile(root: &Path) -> Result<()> {
    let body = concat!(
        r#"{"profiles":{},"clientToken":"","authenticationDatabase":{},"#,
        r#""launcherVersion":{"name":"NewBee Launcher","format":21},"#,
        r#""settings":{},"version":3}"#
    );
    for name in ["launcher_profiles.json", "launcher_profiles_microsoft_store.json"] {
        let path = root.join(name);
        if !path.exists() {
            std::fs::write(&path, body)
                .with_context(|| format!("写入 {}", path.display()))?;
        }
    }
    Ok(())
}

async fn run_installer(
    ctx: &Ctx,
    java: &Path,
    jar: &Path,
    root: &Path,
) -> Result<()> {
    ensure_launcher_profile(root)?;
    ctx.log(format!(
        "运行安装器: {} -jar {} --installClient {}",
        java.display(),
        jar.display(),
        root.display()
    ));
    let mut cmd = tokio::process::Command::new(java);
    cmd.arg("-jar")
        .arg(jar)
        .arg("--installClient")
        .arg(root)
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // keep the console window hidden when launched from the GUI
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().with_context(|| format!("启动 {}", java.display()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let h1 = stdout.map(|s| {
        let tx = ctx.tx.clone();
        tokio::spawn(async move { pump(s, tx).await })
    });
    let h2 = stderr.map(|s| {
        let tx = ctx.tx.clone();
        tokio::spawn(async move { pump(s, tx).await })
    });
    let status = child.wait().await?;
    if let Some(h) = h1 {
        let _ = h.await;
    }
    if let Some(h) = h2 {
        let _ = h.await;
    }
    if !status.success() {
        bail!("安装器失败，退出码 {:?}", status.code());
    }
    Ok(())
}

fn scan_new_version(layout: &crate::paths::Layout, needle: &str, mc: &str) -> Option<String> {
    let mut hits: Vec<String> = crate::install::installed_versions(layout)
        .into_iter()
        .filter(|id| {
            let lower = id.to_lowercase();
            lower.contains("forge") && (lower.contains(&needle.to_lowercase()) || lower.contains(mc))
        })
        .collect();
    hits.sort();
    hits.pop()
}

pub async fn install(
    ctx: &Ctx,
    mc: &str,
    forge_version: Option<&str>,
    concurrency: usize,
    java: Option<&Path>,
) -> Result<String> {
    let fv = match forge_version.map(|s| s.trim().to_string()) {
        Some(v) if !v.is_empty() => v,
        _ => {
            let list = list_forge(ctx, mc).await?;
            list.iter()
                .find(|l| l.stable)
                .or_else(|| list.first())
                .map(|l| l.version.clone())
                .ok_or_else(|| anyhow!("{mc} 没有可用的 Forge 版本"))?
        }
    };

    // The installer patches the vanilla jar, so vanilla must be present first.
    ctx.tx.stage(format!("准备原版 {mc}"));
    crate::install::prepare(ctx, mc, concurrency).await?;

    let file = format!("forge-{mc}-{fv}-installer.jar");
    let url = format!("{FORGE_MAVEN}/{mc}-{fv}/{file}");
    let dest = ctx.layout.cache_dir().join(&file);
    ctx.tx.stage(format!("下载 Forge 安装器 {fv}"));
    crate::net::download_many(
        &ctx.client,
        vec![crate::net::DlTask::new(url, dest.clone(), None).with_label(file.clone())],
        concurrency,
        &ctx.tx,
        "Forge 安装器",
    )
    .await?;

    let java = match java {
        Some(j) if j.is_file() => j.to_path_buf(),
        _ => super::installer_java(ctx, concurrency).await?,
    };

    ctx.tx.stage(format!("Forge {fv} 安装中（首次会下载并修补大量库，请耐心等待）"));
    run_installer(ctx, &java, &dest, &ctx.layout.shared()).await?;

    let expected = format!("{mc}-forge-{fv}");
    if ctx.layout.version_json(&expected).is_file() {
        ctx.log(format!("Forge 安装完成: {expected}"));
        return Ok(expected);
    }
    if let Some(id) = scan_new_version(&ctx.layout, &fv, mc) {
        ctx.log(format!("Forge 安装完成: {id}"));
        return Ok(id);
    }
    Err(anyhow!(
        "安装器跑完了，但没在 versions/ 里找到 Forge 版本（期望 {expected}）。请看日志里的安装器输出。"
    ))
}

pub async fn install_neoforge(
    ctx: &Ctx,
    mc: &str,
    loader_version: Option<&str>,
    concurrency: usize,
    java: Option<&Path>,
) -> Result<String> {
    let nv = match loader_version.map(|s| s.trim().to_string()) {
        Some(v) if !v.is_empty() => v,
        _ => {
            let list = list_neoforge(ctx, mc).await?;
            list.iter()
                .find(|l| l.stable)
                .or_else(|| list.first())
                .map(|l| l.version.clone())
                .ok_or_else(|| anyhow!("{mc} 没有可用的 NeoForge 版本"))?
        }
    };

    ctx.tx.stage(format!("准备原版 {mc}"));
    crate::install::prepare(ctx, mc, concurrency).await?;

    let file = format!("neoforge-{nv}-installer.jar");
    let url = format!("{NEOFORGE_MAVEN}/{nv}/{file}");
    let dest = ctx.layout.cache_dir().join(&file);
    ctx.tx.stage(format!("下载 NeoForge 安装器 {nv}"));
    crate::net::download_many(
        &ctx.client,
        vec![crate::net::DlTask::new(url, dest.clone(), None).with_label(file.clone())],
        concurrency,
        &ctx.tx,
        "NeoForge 安装器",
    )
    .await?;

    let java = match java {
        Some(j) if j.is_file() => j.to_path_buf(),
        _ => super::installer_java(ctx, concurrency).await?,
    };

    ctx.tx.stage(format!("NeoForge {nv} 安装中"));
    run_installer(ctx, &java, &dest, &ctx.layout.shared()).await?;

    for candidate in [
        format!("neoforge-{nv}"),
        format!("{mc}-neoforge-{nv}"),
    ] {
        if ctx.layout.version_json(&candidate).is_file() {
            ctx.log(format!("NeoForge 安装完成: {candidate}"));
            return Ok(candidate);
        }
    }
    if let Some(id) = scan_new_version(&ctx.layout, &nv, mc) {
        ctx.log(format!("NeoForge 安装完成: {id}"));
        return Ok(id);
    }
    Err(anyhow!(
        "安装器跑完了，但没在 versions/ 里找到 NeoForge 版本。请看日志里的安装器输出。"
    ))
}

#[allow(dead_code)]
fn _unused(_: PathBuf) {}