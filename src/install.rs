//! Installing game files: version JSONs, client jars, libraries, natives,
//! assets, log configs and Mojang Java runtimes.

use crate::mojang::{self, Artifact, FeatureSet, AssetIndex, RuntimeManifest, VersionJson};
use crate::net::{self, DlTask};
use crate::paths::Layout;
use crate::util::MavenName;
use crate::Ctx;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Everything the launcher needs to build a command line.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub version: VersionJson,
    pub client_jar: PathBuf,
    pub classpath: Vec<PathBuf>,
    pub native_jars: Vec<(PathBuf, Vec<String>)>,
    pub assets_dir: PathBuf,
    pub asset_index: String,
    pub log_config: Option<PathBuf>,
    /// `libraries/` root — Forge's module path is expressed relative to it.
    pub library_dir: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryPlan {
    pub classpath: Vec<PathBuf>,
    pub native_jars: Vec<(PathBuf, Vec<String>)>,
    pub tasks: Vec<DlTask>,
}

fn arch_bits() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "64",
        "aarch64" => "arm64",
        _ => "32",
    }
}

/// Turn the (already flattened) version JSON into a download plan + classpath.
pub fn plan_libraries(layout: &Layout, version: &VersionJson, features: &FeatureSet) -> LibraryPlan {
    let mut plan = LibraryPlan::default();
    for lib in &version.libraries {
        if !mojang::rules_allow(&lib.rules, features) {
            continue;
        }
        let maven = MavenName::parse(&lib.name).ok();

        if let Some(dl) = &lib.downloads {
            if let Some(art) = &dl.artifact {
                let rel = art
                    .path
                    .clone()
                    .or_else(|| maven.as_ref().map(|m| m.path()))
                    .unwrap_or_default();
                if rel.is_empty() {
                    continue;
                }
                let dest = layout.libraries_dir().join(&rel);
                if !art.url.is_empty() {
                    plan.tasks.push(task_from_artifact(art, &dest, &lib.name));
                }
                plan.classpath.push(dest);
            }
            // Legacy natives live in classifiers and must be unpacked.
            if let (Some(natives), Some(classifiers)) = (&lib.natives, &dl.classifiers) {
                if let Some(classifier) = natives.get(mojang::os_name()) {
                    let classifier = classifier.replace("${arch}", arch_bits());
                    if let Some(art) = classifiers.get(&classifier) {
                        let rel = art
                            .path
                            .clone()
                            .unwrap_or_else(|| format!("{}-{}.jar", lib.name.replace(':', "-"), classifier));
                        let dest = layout.libraries_dir().join(&rel);
                        if !art.url.is_empty() {
                            plan.tasks.push(task_from_artifact(art, &dest, &lib.name));
                        }
                        let exclude = lib
                            .extract
                            .as_ref()
                            .map(|e| e.exclude.clone())
                            .unwrap_or_else(|| vec!["META-INF/".to_string()]);
                        plan.native_jars.push((dest, exclude));
                    }
                }
            }
        } else if let (Some(base), Some(m)) = (&lib.url, &maven) {
            // Fabric / Quilt style: bare `url` + `name`.
            let rel = m.path();
            let dest = layout.libraries_dir().join(&rel);
            let url = format!("{}{}", ensure_slash(base), rel);
            plan.tasks.push(DlTask::new(url, dest.clone(), None).with_label(lib.name.clone()));
            plan.classpath.push(dest);
        }
    }
    plan
}

fn ensure_slash(base: &str) -> String {
    if base.ends_with('/') {
        base.to_string()
    } else {
        format!("{base}/")
    }
}

fn task_from_artifact(art: &Artifact, dest: &Path, label: &str) -> DlTask {
    let mut t = DlTask::new(art.url.clone(), dest.to_path_buf(), None).with_label(label.to_string());
    if !art.sha1.is_empty() {
        t.sha1 = Some(art.sha1.clone());
    }
    if art.size > 0 {
        t.size = Some(art.size);
    }
    t
}

/// Make sure every JSON in the inheritance chain, plus all client jars, are local.
pub async fn install_chain(
    ctx: &Ctx,
    manifest: &mojang::VersionManifest,
    version_id: &str,
    concurrency: usize,
) -> Result<()> {
    let mut cursor = Some(version_id.to_string());
    let mut seen: Vec<String> = Vec::new();
    while let Some(id) = cursor {
        if seen.contains(&id) {
            bail!("版本继承链出现循环: {seen:?} -> {id}");
        }
        seen.push(id.clone());
        let v = mojang::ensure_version_json(ctx, manifest, &id).await?;
        if let Some(dl) = v.downloads.as_ref().and_then(|d| d.client.as_ref()) {
            let dest = ctx.layout.version_jar(&id);
            ctx.log(format!("下载客户端 {id}.jar"));
            net::download_many(
                &ctx.client,
                vec![task_from_artifact(dl, &dest, &format!("{id}.jar"))],
                concurrency,
                &ctx.tx,
                "客户端 jar",
            )
            .await?;
        }
        cursor = v.inherits_from.clone();
    }
    Ok(())
}

async fn ensure_log_config(ctx: &Ctx, version: &VersionJson, concurrency: usize) -> Option<PathBuf> {
    let logging = version.logging.as_ref()?.get("client")?;
    let dir = ctx.layout.assets_log_configs_dir();
    let dest = dir.join(&logging.file.id);
    if !logging.file.url.is_empty() {
        let mut t = DlTask::new(
            logging.file.url.clone(),
            dest.clone(),
            Some(logging.file.sha1.clone()),
        )
        .with_label(format!("log4j {}", logging.file.id));
        t.size = Some(logging.file.size);
        let _ = net::download_many(
            &ctx.client,
            vec![t],
            concurrency,
            &ctx.tx,
            "日志配置",
        )
        .await;
    }
    if dest.exists() {
        Some(dest)
    } else {
        None
    }
}

/// Download the asset index and every object; returns the `--assetsDir` root.
pub async fn ensure_assets(
    ctx: &Ctx,
    version: &VersionJson,
    concurrency: usize,
) -> Result<(PathBuf, String)> {
    let Some(idx) = &version.asset_index else {
        // pre-1.6 versions have no index at all
        return Ok((ctx.layout.assets_dir(), "legacy".to_string()));
    };
    let index_path = ctx.layout.assets_indexes_dir().join(format!("{}.json", idx.id));
    if index_path.exists() {
        if crate::util::sha1_file(&index_path)
            .map(|h| !idx.sha1.is_empty() && h.eq_ignore_ascii_case(&idx.sha1))
            .unwrap_or(false)
        {
            // already good
        } else {
            let _ = std::fs::remove_file(&index_path);
        }
    }
    if !index_path.exists() {
        let mut t = DlTask::new(idx.url.clone(), index_path.clone(), None)
            .with_label(format!("{}.json 资源索引", idx.id));
        if !idx.sha1.is_empty() {
            t.sha1 = Some(idx.sha1.clone());
        }
        net::download_many(&ctx.client, vec![t], concurrency, &ctx.tx, "资源索引").await?;
    }
    let text = std::fs::read_to_string(&index_path)
        .with_context(|| format!("reading {}", index_path.display()))?;
    let index: AssetIndex = serde_json::from_str(&text)
        .with_context(|| format!("parsing asset index {}", idx.id))?;

    let objects_dir = ctx.layout.assets_objects_dir();
    let mut tasks = Vec::with_capacity(index.objects.len());
    for (name, obj) in &index.objects {
        if obj.hash.len() < 2 {
            continue;
        }
        let sub = &obj.hash[..2];
        let dest = objects_dir.join(sub).join(&obj.hash);
        let url = format!("{}{}/{}", mojang::ASSET_BASE_URL, sub, obj.hash);
        let mut t = DlTask::new(url, dest, Some(obj.hash.clone())).with_label(name.clone());
        t.size = Some(obj.size);
        tasks.push(t);
    }
    ctx.log(format!("资源文件 {}/{} 个", tasks.len(), index.objects.len()));
    net::download_many(&ctx.client, tasks, concurrency, &ctx.tx, "资源文件").await?;

    if index.virtual_ == Some(true) || index.map_to_resources == Some(true) {
        let vdir = ctx.layout.assets_virtual_dir(&idx.id);
        ctx.log(format!("构建虚拟资源目录 {}", vdir.display()));
        for (name, obj) in &index.objects {
            let src = objects_dir.join(&obj.hash[..2]).join(&obj.hash);
            let dst = vdir.join(name);
            if dst.exists() || !src.exists() {
                continue;
            }
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let _ = std::fs::copy(&src, &dst);
        }
        return Ok((vdir, idx.id.clone()));
    }
    Ok((ctx.layout.assets_dir(), idx.id.clone()))
}

/// Full install/verify of a version and everything it needs.
pub async fn prepare(ctx: &Ctx, version_id: &str, concurrency: usize) -> Result<Prepared> {
    let manifest = mojang::fetch_manifest(ctx).await?;
    ctx.tx.stage(format!("安装 {version_id}"));
    install_chain(ctx, &manifest, version_id, concurrency).await?;

    let version = mojang::load_version(ctx, version_id).await?;
    let features = FeatureSet::default();
    let plan = plan_libraries(&ctx.layout, &version, &features);
    if !plan.tasks.is_empty() {
        net::download_many(&ctx.client, plan.tasks.clone(), concurrency, &ctx.tx, "依赖库").await?;
    }

    let (assets_dir, asset_index) = ensure_assets(ctx, &version, concurrency).await?;
    let log_config = ensure_log_config(ctx, &version, concurrency).await;

    let client_jar_id = version
        .client_jar_id
        .clone()
        .unwrap_or_else(|| version.root_id(version_id));
    let mut client_jar = ctx.layout.version_jar(&client_jar_id);
    if !client_jar.exists() {
        // fall back to the requested id (loader profile without a parent client)
        let alt = ctx.layout.version_jar(version_id);
        if alt.exists() {
            client_jar = alt;
        }
    }
    if !client_jar.exists() {
        bail!(
            "找不到客户端 jar：{}（请重新安装 {version_id}）",
            client_jar.display()
        );
    }

    let mut classpath = plan.classpath;
    // Forge/NeoForge 1.17+ boot through `BootstrapLauncher`, which builds the
    // `minecraft` module itself from the patched client jar it finds through
    // `-DlibraryDirectory`. Putting the vanilla jar on the classpath as well
    // makes two modules own the same packages and the game refuses to start.
    let bootstrap_launcher = version
        .main_class
        .as_deref()
        .map(|m| m.contains("bootstraplauncher"))
        .unwrap_or(false);
    if !bootstrap_launcher {
        classpath.push(client_jar.clone());
    }

    Ok(Prepared {
        version,
        client_jar,
        classpath,
        native_jars: plan.native_jars,
        assets_dir,
        asset_index,
        log_config,
        library_dir: ctx.layout.libraries_dir(),
    })
}

/// Unpack the legacy native jars into `natives_dir` (idempotent).
pub fn extract_natives(native_jars: &[(PathBuf, Vec<String>)], natives_dir: &Path) -> Result<usize> {
    std::fs::create_dir_all(natives_dir)?;
    let mut count = 0usize;
    for (jar, exclude) in native_jars {
        if !jar.exists() {
            continue;
        }
        let file = std::fs::File::open(jar)
            .with_context(|| format!("opening native jar {}", jar.display()))?;
        let mut zip = zip::ZipArchive::new(file)
            .with_context(|| format!("reading native jar {}", jar.display()))?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let Some(name) = entry.enclosed_name() else { continue };
            let name_str = name.to_string_lossy().replace('\\', "/");
            if exclude.iter().any(|e| name_str.starts_with(e)) {
                continue;
            }
            if name_str.ends_with(".git") || name_str.ends_with(".sha1") {
                continue;
            }
            let out_path = natives_dir.join(&*name);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut out = std::fs::File::create(&out_path)?;
            std::io::copy(&mut entry, &mut out)?;
            count += 1;
        }
    }
    Ok(count)
}

// ------------------------------------------------------------------- java

/// Locate (or download) a Java runtime for `major`.
pub async fn ensure_java(
    ctx: &Ctx,
    component: Option<&str>,
    major: u32,
    concurrency: usize,
) -> Result<PathBuf> {
    let platform = mojang::runtime_platform();
    let component = component
        .map(|s| s.to_string())
        .unwrap_or_else(|| default_component(major));

    let dir = ctx.layout.java_dir().join(&component).join(&platform);
    let exe = dir.join("bin").join(if cfg!(windows) { "java.exe" } else { "java" });
    if exe.is_file() {
        return Ok(exe);
    }

    ctx.log(format!("下载 Java 运行时 {component} ({platform})"));
    let cat = mojang::fetch_runtime_catalogue(ctx).await?;
    let entry = mojang::pick_runtime(&cat, &platform, &component)
        .ok_or_else(|| mojang::platform_bail(&platform))?;
    let manifest: RuntimeManifest = net::get_json(&ctx.client, &entry.manifest.url).await?;

    let mut tasks = Vec::new();
    for (rel, f) in &manifest.files {
        match f.kind.as_str() {
            "directory" => {
                std::fs::create_dir_all(dir.join(rel))?;
            }
            "file" => {
                if let Some(raw) = f.downloads.get("raw") {
                    let mut t = DlTask::new(
                        raw.url.clone(),
                        dir.join(rel),
                        Some(raw.sha1.clone()),
                    )
                    .with_label(rel.clone());
                    t.size = Some(raw.size);
                    tasks.push(t);
                }
            }
            _ => {}
        }
    }
    if tasks.is_empty() {
        bail!("Java 运行时清单为空（{component}）");
    }
    net::download_many(&ctx.client, tasks, concurrency, &ctx.tx, "Java 运行时").await?;
    if !exe.is_file() {
        bail!("Java 下载完成但可执行文件不在 {}", exe.display());
    }
    Ok(exe)
}

fn default_component(major: u32) -> String {
    match major {
        0..=8 => "jre-legacy",
        9..=16 => "java-runtime-alpha",
        17..=20 => "java-runtime-beta",
        21..=24 => "java-runtime-delta",
        // Mojang ships Java 25 as java-runtime-epsilon (verified against all.json)
        _ => "java-runtime-epsilon",
    }
    .to_string()
}

/// Version ids present on disk (sorted newest-first by mtime).
pub fn installed_versions(layout: &Layout) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(layout.versions_dir()) {
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_dir() {
                continue;
            }
            let Some(name) = p.file_name().map(|s| s.to_string_lossy().to_string()) else {
                continue;
            };
            if p.join(format!("{name}.json")).is_file() {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

/// Quick sanity summary used by the CLI `list` command.
pub fn describe_installed(layout: &Layout, id: &str) -> Option<(String, bool)> {
    let path = layout.version_json(id);
    let text = std::fs::read_to_string(path).ok()?;
    let v: VersionJson = serde_json::from_str(&text).ok()?;
    Some((v.root_id(id), layout.version_jar(id).exists()))
}

