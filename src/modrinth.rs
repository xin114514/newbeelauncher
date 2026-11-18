//! Modrinth API v2: browse and install mods, resource packs and shaders.
//!
//! Modrinth needs no API key, which makes it the best default content source.

use crate::net;
use crate::paths::Layout;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Clone, Deserialize)]
pub struct SearchHit {
    pub project_id: String,
    #[serde(default)]
    pub slug: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub downloads: u64,
    #[serde(default)]
    pub icon_url: Option<String>,
    #[serde(default)]
    pub project_type: String,
    #[serde(default)]
    pub versions: Vec<String>,
    #[serde(default)]
    pub categories: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub hits: Vec<SearchHit>,
    #[serde(default)]
    pub total_hits: u64,
    #[serde(default)]
    pub offset: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModFile {
    pub url: String,
    pub filename: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub hashes: HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Dependency {
    #[serde(default)]
    pub version_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub dependency_type: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectVersion {
    pub id: String,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version_number: String,
    #[serde(default)]
    pub version_type: String,
    #[serde(default)]
    pub game_versions: Vec<String>,
    #[serde(default)]
    pub loaders: Vec<String>,
    #[serde(default)]
    pub files: Vec<ModFile>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

impl ProjectVersion {
    pub fn primary_file(&self) -> Option<&ModFile> {
        self.files
            .iter()
            .find(|f| f.primary)
            .or_else(|| self.files.first())
    }
}

/// Build the `facets` parameter, e.g. `[["project_type:mod"],["versions:1.21.1"]]`.
pub fn facets(project_type: &str, game_version: Option<&str>, loaders: &[&str]) -> String {
    let mut groups: Vec<String> = Vec::new();
    if !project_type.is_empty() {
        groups.push(format!("[\"project_type:{project_type}\"]"));
    }
    if let Some(gv) = game_version {
        if !gv.is_empty() {
            groups.push(format!("[\"versions:{gv}\"]"));
        }
    }
    if !loaders.is_empty() {
        let inner = loaders
            .iter()
            .map(|l| format!("\"categories:{l}\""))
            .collect::<Vec<_>>()
            .join(",");
        groups.push(format!("[{inner}]"));
    }
    format!("[{}]", groups.join(","))
}

/// One page of search results ("拉取 mod 列表").
pub async fn search(
    ctx: &Ctx,
    query: &str,
    project_type: &str,
    game_version: Option<&str>,
    loaders: &[&str],
    limit: usize,
    offset: usize,
) -> Result<SearchResponse> {
    let facet = facets(project_type, game_version, loaders);
    let url = format!(
        "{API}/search?query={}&facets={}&limit={}&offset={}&index=relevance",
        urlencoding::encode(query),
        urlencoding::encode(&facet),
        limit.clamp(1, 100),
        offset
    );
    let resp: SearchResponse = net::get_json(&ctx.client, &url)
        .await
        .with_context(|| format!("Modrinth 搜索失败: {query}"))?;
    Ok(resp)
}

/// All versions of a project matching a game version / loader set.
pub async fn versions_for(
    ctx: &Ctx,
    project_id: &str,
    game_version: Option<&str>,
    loader: Option<&str>,
) -> Result<Vec<ProjectVersion>> {
    let mut url = format!("{API}/project/{project_id}/version");
    let mut params: Vec<String> = Vec::new();
    if let Some(gv) = game_version {
        if !gv.is_empty() {
            params.push(format!(
                "game_versions={}",
                urlencoding::encode(&format!("[\"{gv}\"]"))
            ));
        }
    }
    if let Some(l) = loader {
        if !l.is_empty() {
            params.push(format!(
                "loaders={}",
                urlencoding::encode(&format!("[\"{l}\"]"))
            ));
        }
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }
    let list: Vec<ProjectVersion> = net::get_json(&ctx.client, &url)
        .await
        .with_context(|| format!("获取 {project_id} 的版本列表失败"))?;
    Ok(list)
}

/// Pick the best version: newest release, then newest beta/alpha.
pub fn pick_best(list: &[ProjectVersion]) -> Option<&ProjectVersion> {
    list.iter()
        .find(|v| v.version_type == "release")
        .or_else(|| list.first())
}

pub fn target_dir(layout: &Layout, instance_id: &str, project_type: &str) -> PathBuf {
    match project_type {
        "resourcepack" => layout.instance_resourcepacks_dir(instance_id),
        "shader" => layout.instance_shaderpacks_dir(instance_id),
        _ => layout.instance_mods_dir(instance_id),
    }
}

async fn download_file(
    ctx: &Ctx,
    file: &ModFile,
    dest_dir: &Path,
    concurrency: usize,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dest_dir)?;
    let dest = dest_dir.join(&file.filename);
    let sha1 = file.hashes.get("sha1").cloned();
    let mut task = net::DlTask::new(file.url.clone(), dest.clone(), sha1).with_label(file.filename.clone());
    task.size = Some(file.size);
    net::download_many(&ctx.client, vec![task], concurrency, &ctx.tx, "下载内容").await?;
    Ok(dest)
}

/// Download one project version, plus its required dependencies.
pub async fn install_version(
    ctx: &Ctx,
    version: &ProjectVersion,
    dest_dir: &Path,
    concurrency: usize,
) -> Result<Vec<PathBuf>> {
    let file = version
        .primary_file()
        .ok_or_else(|| anyhow!("版本 {} 没有可下载文件", version.name))?;
    let path = download_file(ctx, file, dest_dir, concurrency).await?;
    ctx.log(format!(
        "已安装 {} {} -> {}",
        if version.name.is_empty() {
            &version.version_number
        } else {
            &version.name
        },
        version.version_number,
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    Ok(vec![path])
}

/// Resolve + download a project for an instance (recursing into required deps).
pub async fn install_project(
    ctx: &Ctx,
    project_id: &str,
    game_version: &str,
    loader: Option<&str>,
    _project_type: &str,
    dest_dir: &Path,
    concurrency: usize,
) -> Result<Vec<PathBuf>> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = vec![project_id.to_string()];
    let mut out: Vec<PathBuf> = Vec::new();
    let mut depth = 0;

    while let Some(pid) = queue.pop() {
        depth += 1;
        if depth > 32 {
            break;
        }
        if !visited.insert(pid.clone()) {
            continue;
        }
        let list = versions_for(ctx, &pid, Some(game_version), loader).await?;
        let Some(best) = pick_best(&list) else {
            ctx.log(format!("{pid} 没有适配 {game_version} 的版本，跳过"));
            continue;
        };
        out.extend(install_version(ctx, best, dest_dir, concurrency).await?);
        for dep in &best.dependencies {
            if dep.dependency_type == "required" {
                if let Some(dep_project) = &dep.project_id {
                    if !visited.contains(dep_project) {
                        queue.push(dep_project.clone());
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Convenience wrapper used by the "一键安装" button.
pub async fn install_by_project(
    ctx: &Ctx,
    layout: &Layout,
    instance_id: &str,
    project_type: &str,
    project_id: &str,
    game_version: &str,
    loader: Option<&str>,
    concurrency: usize,
) -> Result<Vec<PathBuf>> {
    let dir = target_dir(layout, instance_id, project_type);
    let loader = match project_type {
        "mod" => loader,
        _ => None,
    };
    install_project(
        ctx,
        project_id,
        game_version,
        loader,
        project_type,
        &dir,
        concurrency,
    )
    .await
}

/// Delete a downloaded file from an instance's content folder.
pub fn remove_file(dir: &Path, name: &str) -> Result<()> {
    let path = dir.join(name);
    if !path.starts_with(dir) {
        bail!("非法路径");
    }
    std::fs::remove_file(&path).with_context(|| format!("删除 {}", path.display()))?;
    Ok(())
}