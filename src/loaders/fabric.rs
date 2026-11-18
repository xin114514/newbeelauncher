//! Fabric and Quilt: both expose a ready-made launch profile that inherits the
//! vanilla version, so installation is "fetch profile JSON, then install it".

use super::LoaderChoice;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;

const FABRIC_META: &str = "https://meta.fabricmc.net";
const QUILT_META: &str = "https://meta.quiltmc.org";

#[derive(Debug, Deserialize)]
struct LoaderEntry {
    #[serde(default)]
    loader: Option<LoaderInfo>,
}

#[derive(Debug, Deserialize)]
struct LoaderInfo {
    #[serde(default)]
    version: String,
    #[serde(default)]
    stable: bool,
}

fn base(quilt: bool) -> &'static str {
    if quilt {
        QUILT_META
    } else {
        FABRIC_META
    }
}

fn meta_version(quilt: bool) -> &'static str {
    if quilt {
        "v3"
    } else {
        "v2"
    }
}

pub async fn list_loaders(ctx: &Ctx, mc: &str, quilt: bool) -> Result<Vec<LoaderChoice>> {
    let url = format!(
        "{}/{}/versions/loader/{}",
        base(quilt),
        meta_version(quilt),
        mc
    );
    let text = crate::net::get_text(&ctx.client, &url)
        .await
        .with_context(|| format!("获取 {url}"))?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let list = value
        .as_array()
        .ok_or_else(|| anyhow!("意外的 loader 列表格式"))?;
    let mut out = Vec::new();
    for item in list {
        // Both Fabric (`v2`) and Quilt (`v3`) nest the data under `loader`.
        if let Ok(e) = serde_json::from_value::<LoaderEntry>(item.clone()) {
            if let Some(l) = e.loader {
                if !l.version.is_empty() {
                    out.push(LoaderChoice {
                        version: l.version,
                        stable: l.stable,
                    });
                }
            }
        }
    }
    if out.is_empty() {
        bail!("{mc} 没有可用的 {} loader 版本", if quilt { "Quilt" } else { "Fabric" });
    }
    // newest first (the meta API returns newest first already, but be explicit)
    out.sort_by(|a, b| version_cmp(&b.version, &a.version));
    Ok(out)
}

fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u32> {
        s.split(|c: char| !c.is_ascii_digit())
            .filter(|p| !p.is_empty())
            .filter_map(|p| p.parse::<u32>().ok())
            .collect()
    };
    parse(a).cmp(&parse(b))
}

async fn latest_loader(ctx: &Ctx, mc: &str, quilt: bool) -> Result<String> {
    let list = list_loaders(ctx, mc, quilt).await?;
    list.iter()
        .find(|l| l.stable)
        .or_else(|| list.first())
        .map(|l| l.version.clone())
        .ok_or_else(|| anyhow!("没有可用 loader"))
}

/// Download the profile JSON, write it into `versions/`, then install everything
/// it (and its parent) needs. Returns the version id.
pub async fn install(
    ctx: &Ctx,
    mc: &str,
    loader_version: Option<&str>,
    concurrency: usize,
    quilt: bool,
) -> Result<String> {
    let loader = match loader_version.map(|s| s.trim().to_string()) {
        Some(v) if !v.is_empty() => v,
        _ => latest_loader(ctx, mc, quilt).await?,
    };
    ctx.tx
        .stage(format!("安装 {} {} / {mc}", if quilt { "Quilt" } else { "Fabric" }, loader));

    let url = format!(
        "{}/{}/versions/loader/{}/{}/profile/json",
        base(quilt),
        meta_version(quilt),
        mc,
        loader
    );
    let text = crate::net::get_text(&ctx.client, &url)
        .await
        .with_context(|| format!("获取 {url}"))?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let id = value
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("profile 缺少 id"))?
        .to_string();

    let dir = ctx.layout.version_dir(&id);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(ctx.layout.version_json(&id), serde_json::to_string_pretty(&value)?)?;
    ctx.log(format!("已写入版本 profile: {id}"));

    // `prepare` walks the inheritance chain (downloading vanilla) and fetches
    // libraries + assets, so this one call makes the instance launchable.
    crate::install::prepare(ctx, &id, concurrency).await?;
    Ok(id)
}