//! Mojang metadata: version manifest, version JSONs (with `inheritsFrom`
//! flattening), assets indexes and the Java runtime catalogue.

use crate::net;
use crate::Ctx;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const ASSET_BASE_URL: &str = "https://resources.download.minecraft.net/";
pub const LIBRARY_BASE_URL: &str = "https://libraries.minecraft.net/";
pub const JAVA_RUNTIME_ALL_URL: &str = "https://launchermeta.mojang.com/v1/products/java-runtime/2ec0cc96c44e5a76b9c8b7c39df7210883d12871/all.json";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Latest {
    pub release: String,
    #[serde(default)]
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub release_time: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<VersionSummary>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<serde_json::Value>,
    #[serde(default)]
    pub jvm: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Artifact {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    #[serde(default)]
    pub natives: Option<HashMap<String, String>>,
    #[serde(default)]
    pub rules: Option<Vec<Rule>>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub extract: Option<Extract>,
    #[serde(default)]
    pub checksums: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetIndexRef {
    pub id: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub total_size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct VersionDownloads {
    #[serde(default)]
    pub client: Option<Artifact>,
    #[serde(default)]
    pub server: Option<Artifact>,
    #[serde(default, rename = "client_mappings")]
    pub client_mappings: Option<Artifact>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct JavaVersion {
    #[serde(default)]
    pub component: String,
    #[serde(default)]
    pub major_version: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LogFile {
    pub id: String,
    #[serde(default)]
    pub sha1: String,
    #[serde(default)]
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Logging {
    pub argument: String,
    pub file: LogFile,
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default)]
    pub assets: Option<String>,
    #[serde(default)]
    pub downloads: Option<VersionDownloads>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    #[serde(default)]
    pub logging: Option<HashMap<String, Logging>>,
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub release_time: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
    #[serde(default)]
    pub compliance_level: Option<u32>,
    #[serde(default)]
    pub minimum_launcher_version: Option<u32>,
    /// Filled in while flattening: which version's directory holds the client jar.
    #[serde(skip)]
    pub client_jar_id: Option<String>,
}

impl VersionJson {
    pub fn java_major(&self) -> u32 {
        self.java_version
            .as_ref()
            .map(|j| j.major_version)
            .filter(|m| *m > 0)
            .unwrap_or(8)
    }

    pub fn java_component(&self) -> Option<String> {
        self.java_version
            .as_ref()
            .map(|j| j.component.clone())
            .filter(|c| !c.is_empty())
    }

    pub fn is_modern(&self) -> bool {
        self.arguments.is_some()
    }

    /// Client jar id for the *parent* of a loader profile (`1.21.1`).
    pub fn root_id(&self, fallback: &str) -> String {
        self.inherits_from.clone().unwrap_or_else(|| fallback.to_string())
    }
}

pub async fn fetch_manifest(ctx: &Ctx) -> Result<VersionManifest> {
    let cache = ctx.layout.meta_dir().join("version_manifest_v2.json");
    match net::get_json::<VersionManifest>(&ctx.client, VERSION_MANIFEST_URL).await {
        Ok(m) => {
            let _ = std::fs::create_dir_all(ctx.layout.meta_dir());
            if let Ok(s) = serde_json::to_string(&m) {
                let _ = std::fs::write(&cache, s);
            }
            Ok(m)
        }
        Err(e) => {
            if cache.exists() {
                ctx.log(format!("版本清单拉取失败，使用本地缓存: {e}"));
                let text = std::fs::read_to_string(&cache)?;
                Ok(serde_json::from_str(&text)?)
            } else {
                Err(e)
            }
        }
    }
}

pub fn find_version<'a>(manifest: &'a VersionManifest, id: &str) -> Option<&'a VersionSummary> {
    manifest.versions.iter().find(|v| v.id == id)
}

pub fn releases(manifest: &VersionManifest) -> Vec<&VersionSummary> {
    manifest
        .versions
        .iter()
        .filter(|v| v.kind == "release")
        .collect()
}

/// Download (or refresh) `versions/<id>/<id>.json` and return the raw child JSON.
pub async fn ensure_version_json(ctx: &Ctx, manifest: &VersionManifest, id: &str) -> Result<VersionJson> {
    let path = ctx.layout.version_json(id);
    if path.exists() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<VersionJson>(&text) {
                return Ok(v);
            }
        }
    }
    let summary = find_version(manifest, id)
        .ok_or_else(|| anyhow!("{id} 不在官方版本清单里"))?
        .clone();
    let text = net::get_text(&ctx.client, &summary.url).await?;
    let value: VersionJson = serde_json::from_str(&text)
        .with_context(|| format!("parsing version json for {id}"))?;
    std::fs::create_dir_all(ctx.layout.version_dir(id))?;
    std::fs::write(&path, serde_json::to_string_pretty(&value)?)?;
    Ok(value)
}

/// Load an installed version by id and flatten `inheritsFrom`.
pub async fn load_version(ctx: &Ctx, id: &str) -> Result<VersionJson> {
    let mut chain: Vec<VersionJson> = Vec::new();
    let mut cursor = Some(id.to_string());
    while let Some(cid) = cursor {
        let path = ctx.layout.version_json(&cid);
        let text = std::fs::read_to_string(&path).with_context(|| {
            format!(
                "缺少版本 {cid} 的 JSON（{}），请先安装该版本",
                path.display()
            )
        })?;
        let mut v: VersionJson = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        if v.id.is_empty() {
            v.id = cid.clone();
        }
        if v.downloads.as_ref().and_then(|d| d.client.as_ref()).is_some() {
            v.client_jar_id = Some(v.id.clone());
        }
        cursor = v.inherits_from.clone();
        chain.push(v);
    }
    let mut merged = chain.pop().ok_or_else(|| anyhow!("空版本链"))?;
    for child in chain.into_iter().rev() {
        merged = merge_versions(merged, child);
    }
    merged.id = id.to_string();
    Ok(merged)
}

fn merge_versions(parent: VersionJson, child: VersionJson) -> VersionJson {
    let mut libraries = parent.libraries;
    for lib in &child.libraries {
        match libraries.iter().position(|p| p.name == lib.name) {
            Some(pos) => libraries[pos] = lib.clone(),
            None => libraries.push(lib.clone()),
        }
    }
    let mut game = parent
        .arguments
        .as_ref()
        .map(|a| a.game.clone())
        .unwrap_or_default();
    let mut jvm = parent
        .arguments
        .as_ref()
        .map(|a| a.jvm.clone())
        .unwrap_or_default();
    if let Some(ca) = &child.arguments {
        game.extend(ca.game.clone());
        jvm.extend(ca.jvm.clone());
    }
    let child_has_client = child
        .downloads
        .as_ref()
        .and_then(|d| d.client.as_ref())
        .is_some();
    let client_jar_id = if child_has_client {
        Some(child.id.clone())
    } else {
        child
            .client_jar_id
            .clone()
            .or_else(|| parent.client_jar_id.clone())
    };
    VersionJson {
        id: child.id,
        inherits_from: None,
        client_jar_id,
        main_class: child.main_class.or(parent.main_class),
        minecraft_arguments: child.minecraft_arguments.or(parent.minecraft_arguments),
        arguments: Some(Arguments { game, jvm }),
        libraries,
        asset_index: child.asset_index.or(parent.asset_index),
        assets: child.assets.or(parent.assets),
        downloads: child.downloads.or(parent.downloads),
        java_version: child.java_version.or(parent.java_version),
        logging: child.logging.or(parent.logging),
        kind: child.kind.or(parent.kind),
        release_time: child.release_time.or(parent.release_time),
        time: child.time.or(parent.time),
        compliance_level: child.compliance_level.or(parent.compliance_level),
        minimum_launcher_version: child
            .minimum_launcher_version
            .or(parent.minimum_launcher_version),
    }
}

// ---------------------------------------------------------------- asset index

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetObject {
    pub hash: String,
    #[serde(default)]
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AssetIndex {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub objects: HashMap<String, AssetObject>,
    #[serde(default, rename = "virtual")]
    pub virtual_: Option<bool>,
    #[serde(default, rename = "map_to_resources")]
    pub map_to_resources: Option<bool>,
}

// ---------------------------------------------------------------- rules

#[derive(Debug, Clone, Default)]
pub struct FeatureSet {
    pub is_demo_user: bool,
    pub has_custom_resolution: bool,
    pub has_quick_plays_support: bool,
    pub is_quick_play_singleplayer: bool,
    pub is_quick_play_multiplayer: bool,
    pub is_quick_play_realms: bool,
}

pub fn os_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "osx"
    } else {
        "linux"
    }
}

pub fn os_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "x86" => "x86",
        "aarch64" => "arm64",
        other => other,
    }
}

fn os_rule_matches(rule: &Rule, features: &FeatureSet) -> bool {
    if let Some(os) = &rule.os {
        if let Some(name) = &os.name {
            if name != os_name() {
                return false;
            }
        }
        if let Some(arch) = &os.arch {
            if arch.as_str() != os_arch() {
                return false;
            }
        }
        if let Some(version) = &os.version {
            if let Ok(re) = regex_lite_match(version) {
                if !re {
                    return false;
                }
            }
        }
    }
    if let Some(feats) = &rule.features {
        for (k, want) in feats {
            let have = match k.as_str() {
                "is_demo_user" => features.is_demo_user,
                "has_custom_resolution" => features.has_custom_resolution,
                "has_quick_plays_support" => features.has_quick_plays_support,
                "is_quick_play_singleplayer" => features.is_quick_play_singleplayer,
                "is_quick_play_multiplayer" => features.is_quick_play_multiplayer,
                "is_quick_play_realms" => features.is_quick_play_realms,
                _ => false,
            };
            if have != *want {
                return false;
            }
        }
    }
    true
}

/// Minimal `os.version` matcher: only the handful of regexes that appear in
/// vanilla JSON (`^10\.`, `^1\.8\.0_`, ...) matter, so a substring check over one
/// cached OS version string is enough and avoids a regex engine.
fn regex_lite_match(version: &str) -> Result<bool> {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let osver = CACHE.get_or_init(|| {
        if cfg!(windows) {
            std::process::Command::new("cmd")
                .args(["/C", "ver"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default()
        } else {
            String::new()
        }
    });
    if osver.is_empty() {
        return Ok(true);
    }
    let needle = version
        .trim()
        .trim_start_matches('^')
        .trim_end_matches('$')
        .replace("\\", "")
        .trim_end_matches('.')
        .to_string();
    if needle.is_empty() {
        return Ok(true);
    }
    Ok(osver.contains(&needle))
}

/// Vanilla rule semantics: last matching rule wins; no rules means allowed.
pub fn rules_allow(rules: &Option<Vec<Rule>>, features: &FeatureSet) -> bool {
    let Some(rules) = rules else { return true };
    let mut allowed = false;
    for rule in rules {
        if os_rule_matches(rule, features) {
            allowed = rule.action == "allow";
        }
    }
    allowed
}

// ---------------------------------------------------------------- java runtime

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeEntry {
    pub manifest: Artifact,
    #[serde(default)]
    pub version: Option<RuntimeVersion>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeVersion {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub released: String,
}

pub type RuntimeCatalogue = HashMap<String, HashMap<String, Vec<RuntimeEntry>>>;

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeFile {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub downloads: HashMap<String, Artifact>,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RuntimeManifest {
    pub files: HashMap<String, RuntimeFile>,
}

/// `windows-x64`, `mac-os-arm64`, ...
pub fn runtime_platform() -> String {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => "windows-x64".into(),
        ("windows", "x86") => "windows-x86".into(),
        ("windows", "aarch64") => "windows-arm64".into(),
        ("macos", "aarch64") => "mac-os-arm64".into(),
        ("macos", _) => "mac-os".into(),
        (_, "aarch64") => "linux-arm64".into(),
        _ => "linux".into(),
    }
}

pub async fn fetch_runtime_catalogue(ctx: &Ctx) -> Result<RuntimeCatalogue> {
    net::get_json(&ctx.client, JAVA_RUNTIME_ALL_URL).await
}

/// Pick the newest runtime entry for a component and our platform.
pub fn pick_runtime<'a>(
    cat: &'a RuntimeCatalogue,
    platform: &str,
    component: &str,
) -> Option<&'a RuntimeEntry> {
    cat.get(platform)?
        .get(component)?
        .iter()
        .max_by(|a, b| a.manifest.size.cmp(&b.manifest.size))
}

pub fn platform_bail(platform: &str) -> anyhow::Error {
    anyhow!("当前平台 {platform} 没有可下载的 Mojang Java 运行时，请手动在设置里指定 java 路径")
}