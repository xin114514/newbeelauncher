//! Persistent launcher configuration: accounts, isolated instances, settings.

use crate::auth::{Account, AccountKind};
use crate::paths::Layout;
use crate::progress::ProgressSender;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    NeoForge,
}

impl Default for LoaderKind {
    fn default() -> Self {
        LoaderKind::Vanilla
    }
}

impl LoaderKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "vanilla",
            LoaderKind::Fabric => "fabric",
            LoaderKind::Quilt => "quilt",
            LoaderKind::Forge => "forge",
            LoaderKind::NeoForge => "neoforge",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            LoaderKind::Vanilla => "原版",
            LoaderKind::Fabric => "Fabric",
            LoaderKind::Quilt => "Quilt",
            LoaderKind::Forge => "Forge",
            LoaderKind::NeoForge => "NeoForge",
        }
    }

    pub fn all() -> [LoaderKind; 5] {
        [
            LoaderKind::Vanilla,
            LoaderKind::Fabric,
            LoaderKind::Quilt,
            LoaderKind::Forge,
            LoaderKind::NeoForge,
        ]
    }

    /// Modrinth `categories` facet values that match this loader.
    pub fn modrinth_categories(&self) -> &'static [&'static str] {
        match self {
            LoaderKind::Vanilla => &[],
            LoaderKind::Fabric => &["fabric"],
            LoaderKind::Quilt => &["quilt"],
            LoaderKind::Forge => &["forge"],
            LoaderKind::NeoForge => &["neoforge"],
        }
    }
}

fn default_memory() -> u32 {
    4096
}

fn default_concurrency() -> usize {
    16
}

fn default_client_id() -> String {
    crate::auth::DEFAULT_CLIENT_ID.to_string()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Instance {
    pub id: String,
    pub name: String,
    pub mc_version: String,
    #[serde(default)]
    pub loader: LoaderKind,
    #[serde(default)]
    pub loader_version: Option<String>,
    /// Resolved version json id, e.g. `1.21.1` or `fabric-loader-0.16.9-1.21.1`.
    pub version_id: String,
    #[serde(default = "default_memory")]
    pub memory_mb: u32,
    #[serde(default)]
    pub jvm_args: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub last_played: Option<i64>,
    #[serde(default)]
    pub notes: String,
}

impl Instance {
    pub fn new(
        name: impl Into<String>,
        mc_version: impl Into<String>,
        loader: LoaderKind,
        loader_version: Option<String>,
        version_id: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            id: format!(
                "{}-{}",
                sanitize(&name),
                &uuid::Uuid::new_v4().simple().to_string()[..8]
            ),
            name,
            mc_version: mc_version.into(),
            loader,
            loader_version,
            version_id: version_id.into(),
            memory_mb: default_memory(),
            jvm_args: String::new(),
            created_at: crate::util::now_ts(),
            last_played: None,
            notes: String::new(),
        }
    }
}

pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "instance".into()
    } else {
        cleaned
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    #[serde(default)]
    pub java_path: Option<String>,
    /// Absolute path where instances are stored; `None` = `<data dir>/instances`.
    #[serde(default)]
    pub instances_dir: Option<String>,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
    #[serde(default = "default_client_id")]
    pub microsoft_client_id: String,
    #[serde(default)]
    pub curseforge_api_key: Option<String>,
    /// Extra JVM args appended to every launch (e.g. GC flags).
    #[serde(default)]
    pub extra_jvm_args: String,
    /// Global default memory for new instances.
    #[serde(default = "default_memory")]
    pub default_memory_mb: u32,
    #[serde(default)]
    pub close_launcher_on_start: bool,
    #[serde(default = "default_true")]
    pub auto_download_java: bool,
}

fn default_true() -> bool {
    true
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            java_path: None,
            instances_dir: None,
            concurrency: default_concurrency(),
            microsoft_client_id: default_client_id(),
            curseforge_api_key: None,
            extra_jvm_args: String::new(),
            default_memory_mb: default_memory(),
            close_launcher_on_start: false,
            auto_download_java: true,
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct LauncherConfig {
    #[serde(default)]
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub active_account: Option<String>,
    #[serde(default)]
    pub instances: Vec<Instance>,
    #[serde(default)]
    pub settings: Settings,
}

impl LauncherConfig {
    pub fn load(layout: &Layout) -> Result<Self> {
        let path = layout.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        // tolerate a UTF-8 BOM (some editors/tools add one)
        let text = text.trim_start_matches('\u{feff}');
        let mut cfg: Self = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        // Microsoft decommissioned the legacy launcher ids; configs written by an
        // older build must not keep asking for an app that no longer exists.
        const DECOMMISSIONED: [&str; 2] = ["00000000402b5328", "000000004c12ae6f"];
        if DECOMMISSIONED.contains(&cfg.settings.microsoft_client_id.as_str()) {
            cfg.settings.microsoft_client_id = crate::auth::DEFAULT_CLIENT_ID.to_string();
        }
        for a in cfg.accounts.iter_mut() {
            a.unseal();
        }
        Ok(cfg)
    }

    pub fn save(&self, layout: &Layout) -> Result<()> {
        std::fs::create_dir_all(layout.root())?;
        let path = layout.config_file();
        // Keep one generation of history: a bad write (or an accidental wipe) must
        // never be the only copy of accounts + instance records.
        if path.exists() {
            let _ = std::fs::copy(&path, path.with_extension("json.bak"));
        }
        let mut copy = self.clone();
        for a in copy.accounts.iter_mut() {
            a.seal();
        }
        let text = serde_json::to_string_pretty(&copy)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text)?;
        if path.exists() {
            let _ = std::fs::remove_file(&path);
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    pub fn account(&self, uuid: &str) -> Option<&Account> {
        self.accounts.iter().find(|a| a.id == uuid)
    }

    pub fn active(&self) -> Option<&Account> {
        if let Some(id) = &self.active_account {
            if let Some(a) = self.account(id) {
                return Some(a);
            }
        }
        self.accounts.first()
    }

    pub fn activate(&mut self, uuid: &str) {
        if self.accounts.iter().any(|a| a.id == uuid) {
            self.active_account = Some(uuid.to_string());
        }
    }

    pub fn upsert_account(&mut self, account: Account) {
        match self.accounts.iter_mut().find(|a| a.id == account.id) {
            Some(slot) => *slot = account,
            None => self.accounts.push(account),
        }
    }

    pub fn instance(&self, id: &str) -> Option<&Instance> {
        self.instances.iter().find(|i| i.id == id)
    }

    pub fn instance_mut(&mut self, id: &str) -> Option<&mut Instance> {
        self.instances.iter_mut().find(|i| i.id == id)
    }

    pub fn add_instance(&mut self, inst: Instance) -> String {
        let id = inst.id.clone();
        self.instances.push(inst);
        id
    }

    pub fn remove_instance(&mut self, id: &str) -> bool {
        let before = self.instances.len();
        self.instances.retain(|i| i.id != id);
        before != self.instances.len()
    }

    pub fn name_taken(&self, name: &str) -> bool {
        self.instances.iter().any(|i| i.name == name)
    }
}

/// Read the game directory listing for the "mods" tab.
pub fn list_dir(path: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(path) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                out.push(p.file_name().unwrap_or_default().to_string_lossy().to_string());
            }
        }
    }
    out.sort();
    out
}

pub fn default_loader_for(mc: &str) -> LoaderKind {
    // 1.20.5+ ships with a modern java; NeoForge replaced Forge for 1.20.2+
    let _ = mc;
    LoaderKind::Fabric
}

pub fn is_offline_name_valid(name: &str) -> bool {
    let n = name.trim();
    (3..=16).contains(&n.len()) && n.chars().all(|c| c.is_alphanumeric() || c == '_')
}

pub fn account_kind_label(kind: &AccountKind) -> &'static str {
    match kind {
        AccountKind::Microsoft => "微软账号",
        AccountKind::Offline => "离线账号",
    }
}

// -------------------------------------------------------------- instance store

/// Delete one instance's folder (`instances/<id>`) from disk.
///
/// Shared files (`shared/versions`, `libraries`, `assets`) are deliberately left
/// alone: several instances can point at the same loader build.
pub fn delete_instance_dir(layout: &Layout, id: &str) -> Result<Option<PathBuf>> {
    let dir = layout.instance_dir(id);
    if !dir.exists() {
        return Ok(None);
    }
    std::fs::remove_dir_all(&dir)
        .with_context(|| {
            format!(
                "删除 {} 失败（游戏正在运行或文件被占用？请先关闭游戏）",
                dir.display()
            )
        })?;
    Ok(Some(dir))
}

/// Check that `dir` can be used as the instance store (absolute, creatable, writable).
pub fn validate_instances_dir(dir: &Path) -> Result<()> {
    if dir.as_os_str().is_empty() {
        bail!("路径不能为空");
    }
    if !dir.is_absolute() {
        bail!("请填绝对路径，例如 D:\\Minecraft\\instances");
    }
    if dir.exists() {
        if !dir.is_dir() {
            bail!("{} 已经存在但不是目录", dir.display());
        }
    } else {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("无法创建 {}", dir.display()))?;
    }
    let probe = dir.join(".nbl-write-test");
    std::fs::write(&probe, b"ok")
        .with_context(|| format!("目录不可写：{}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

/// Move every instance from `old` to `new`. On any failure the already moved
/// folders are put back, so the config and the disk never disagree.
pub fn relocate_instances(old: &Path, new: &Path, tx: &ProgressSender) -> Result<usize> {
    if old == new {
        return Ok(0);
    }
    if new.starts_with(old) {
        bail!("新目录不能位于旧实例目录内部");
    }
    if old.starts_with(new) {
        bail!("新目录不能是旧实例目录的上级");
    }
    std::fs::create_dir_all(new)
        .with_context(|| format!("无法创建 {}", new.display()))?;
    if !old.exists() {
        return Ok(0);
    }
    tx.log(format!(
        "把实例从 {} 迁移到 {}",
        old.display(),
        new.display()
    ));

    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let outcome: Result<()> = (|| {
        for entry in std::fs::read_dir(old)? {
            let entry = entry?;
            let name = entry.file_name();
            let src = entry.path();
            let dst = new.join(&name);
            if dst.exists() {
                // All-or-nothing: moving only some instances and then repointing the
                // config would leave the skipped ones unreachable.
                bail!(
                    "目标位置已经存在 {}，请先清理或换个目录再切换（本次未做任何改动）",
                    name.to_string_lossy()
                );
            }
            tx.log(format!("移动 {}", name.to_string_lossy()));
            move_dir(&src, &dst)?;
            moved.push((src, dst));
        }
        Ok(())
    })();

    if let Err(e) = outcome {
        for (src, dst) in moved.iter().rev() {
            let _ = move_dir(dst, src);
        }
        return Err(e.context("迁移实例失败，已回滚"));
    }
    let _ = std::fs::remove_dir(old);
    Ok(moved.len())
}

/// `fs::rename` when possible (instant on the same volume), otherwise copy + delete.
fn move_dir(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    copy_dir_all(src, dst)?;
    std::fs::remove_dir_all(src)
        .with_context(|| format!("清理旧目录 {}", src.display()))?;
    Ok(())
}

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("复制 {}", from.display()))?;
        }
    }
    Ok(())
}