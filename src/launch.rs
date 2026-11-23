//! Turning an installed version + an instance into a running Minecraft process.

use crate::config::Instance;
use crate::install::Prepared;
use crate::mojang::{FeatureSet, Rule};
use crate::paths::Layout;
use crate::Ctx;
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub instance: Instance,
    pub account_name: String,
    pub account_uuid: String,
    /// Minecraft access token (offline accounts pass "0").
    pub access_token: String,
    pub user_type: String,
    pub java: PathBuf,
    pub prepared: Prepared,
    pub natives_dir: PathBuf,
    pub resolution: Option<(u32, u32)>,
    pub extra_jvm_args: String,
}

pub fn classpath_separator() -> char {
    if cfg!(windows) {
        ';'
    } else {
        ':'
    }
}

fn substitute(arg: &str, vars: &HashMap<&str, String>) -> String {
    let mut s = arg.to_string();
    for (k, v) in vars {
        s = s.replace(&format!("${{{k}}}"), v);
    }
    s
}

fn collect_args(value: &serde_json::Value, features: &FeatureSet, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_args(v, features, out);
            }
        }
        serde_json::Value::Object(map) => {
            let allowed = match map.get("rules") {
                Some(rules) => match serde_json::from_value::<Vec<Rule>>(rules.clone()) {
                    Ok(parsed) => crate::mojang::rules_allow(&Some(parsed), features),
                    Err(_) => false,
                },
                None => true,
            };
            if allowed {
                if let Some(v) = map.get("value") {
                    collect_args(v, features, out);
                }
            }
        }
        _ => {}
    }
}

pub struct CommandLine {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub working_dir: PathBuf,
}

/// Build the full java command (JVM args + game args) for an instance.
pub fn build_command(opts: &LaunchOptions) -> Result<CommandLine> {
    let version = &opts.prepared.version;
    let layout_root = &opts.instance;
    let game_dir = opts
        .natives_dir
        .parent()
        .map(|p| p.join("game"))
        .ok_or_else(|| anyhow!("实例目录异常"))?;
    let _ = layout_root;

    let classpath: String = opts
        .prepared
        .classpath
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(&classpath_separator().to_string());

    let mut vars: HashMap<&str, String> = HashMap::new();
    vars.insert("auth_player_name", opts.account_name.clone());
    vars.insert("version_name", opts.instance.version_id.clone());
    vars.insert("game_directory", game_dir.to_string_lossy().to_string());
    vars.insert(
        "assets_root",
        opts.prepared.assets_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "game_assets",
        opts.prepared.assets_dir.to_string_lossy().to_string(),
    );
    vars.insert("assets_index_name", opts.prepared.asset_index.clone());
    vars.insert("auth_uuid", opts.account_uuid.replace('-', ""));
    vars.insert("auth_access_token", opts.access_token.clone());
    vars.insert("auth_session", format!("token:{}", opts.access_token));
    vars.insert("clientid", "0".to_string());
    vars.insert("auth_xuid", "0".to_string());
    vars.insert("user_type", opts.user_type.clone());
    vars.insert("version_type", "release".to_string());
    vars.insert(
        "natives_directory",
        opts.natives_dir.to_string_lossy().to_string(),
    );
    vars.insert("launcher_name", "newbee-launcher".to_string());
    vars.insert("launcher_version", env!("CARGO_PKG_VERSION").to_string());
    vars.insert("classpath", classpath.clone());
    vars.insert("user_properties", "{}".to_string());
    vars.insert("quickPlayPath", String::new());
    vars.insert("quickPlaySingleplayer", String::new());
    vars.insert("quickPlayMultiplayer", String::new());
    vars.insert("quickPlayRealms", String::new());
    vars.insert(
        "library_directory",
        opts.prepared.library_dir.to_string_lossy().to_string(),
    );
    vars.insert(
        "classpath_separator",
        classpath_separator().to_string(),
    );

    let features = FeatureSet {
        has_custom_resolution: opts.resolution.is_some(),
        ..Default::default()
    };
    if let Some((w, h)) = opts.resolution {
        vars.insert("resolution_width", w.to_string());
        vars.insert("resolution_height", h.to_string());
    } else {
        vars.insert("resolution_width", "854".to_string());
        vars.insert("resolution_height", "480".to_string());
    }

    // --- JVM arguments
    let mut jvm: Vec<String> = Vec::new();
    jvm.push(format!("-Xmx{}M", opts.instance.memory_mb.max(512)));
    jvm.push(format!("-Xms{}M", (opts.instance.memory_mb / 2).max(256)));
    for extra in opts.extra_jvm_args.split_whitespace() {
        jvm.push(extra.to_string());
    }
    for extra in opts.instance.jvm_args.split_whitespace() {
        jvm.push(extra.to_string());
    }
    if let Some(log) = &opts.prepared.log_config {
        jvm.push(format!(
            "-Dlog4j.configurationFile={}",
            log.to_string_lossy()
        ));
    }

    if let Some(args) = &version.arguments {
        let mut collected = Vec::new();
        for v in &args.jvm {
            collect_args(v, &features, &mut collected);
        }
        for a in collected {
            jvm.push(substitute(&a, &vars));
        }
    }

    let has_classpath_flag = jvm
        .iter()
        .any(|a| a == "-cp" || a == "-classpath" || a.starts_with("-cp="));
    let has_library_path = jvm.iter().any(|a| a.starts_with("-Djava.library.path="));
    if !has_library_path {
        jvm.push(format!(
            "-Djava.library.path={}",
            opts.natives_dir.to_string_lossy()
        ));
    }
    if !has_classpath_flag {
        jvm.push("-cp".to_string());
        jvm.push(classpath);
    }

    // --- game arguments
    let mut game: Vec<String> = Vec::new();
    if version.is_modern() {
        let mut collected = Vec::new();
        if let Some(args) = &version.arguments {
            for v in &args.game {
                collect_args(v, &features, &mut collected);
            }
        }
        for a in collected {
            game.push(substitute(&a, &vars));
        }
    } else if let Some(template) = &version.minecraft_arguments {
        for piece in template.split_whitespace() {
            game.push(substitute(piece, &vars));
        }
    } else {
        bail!("版本 {} 既没有 arguments 也没有 minecraftArguments", version.id);
    }

    let main_class = version
        .main_class
        .clone()
        .ok_or_else(|| anyhow!("版本 {} 缺少 mainClass", version.id))?;

    let mut args = jvm;
    args.push(main_class);
    args.extend(game);

    Ok(CommandLine {
        program: opts.java.clone(),
        args,
        working_dir: game_dir,
    })
}

/// Spawn the game, streaming its output into the launcher log, and wait for exit.
pub async fn run(ctx: &Ctx, cmd: CommandLine) -> Result<i32> {
    std::fs::create_dir_all(&cmd.working_dir)?;
    ctx.log(format!(
        "启动: {} {} 个参数 (工作目录 {})",
        cmd.program.display(),
        cmd.args.len(),
        cmd.working_dir.display()
    ));
    // keep the exact command line for support/debugging
    let mut dump = format!("{}", cmd.program.display());
    for a in &cmd.args {
        dump.push(' ');
        if a.contains(' ') {
            dump.push('"');
            dump.push_str(a);
            dump.push('"');
        } else {
            dump.push_str(a);
        }
    }
    let _ = std::fs::write(cmd.working_dir.join("launcher-command.txt"), dump);
    let mut command = tokio::process::Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .current_dir(&cmd.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        // stdout piping keeps the console window from popping up
        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("启动 java 失败: {}", cmd.program.display()))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let tx1 = ctx.tx.clone();
    let tx2 = ctx.tx.clone();
    let handle = std::fs::File::create(cmd.working_dir.join("launcher-output.log")).ok();
    let h1 = stdout.map(|s| tokio::spawn(async move { pump(s, tx1).await }));
    let h2 = stderr.map(|s| tokio::spawn(async move { pump(s, tx2).await }));
    let status = child.wait().await?;
    if let Some(h) = h1 {
        let _ = h.await;
    }
    if let Some(h) = h2 {
        let _ = h.await;
    }
    if let Some(f) = handle {
        drop(f);
    }
    let code = status.code().unwrap_or(-1);
    if status.success() {
        ctx.tx.done(format!("游戏正常退出（{code}）"));
    } else {
        ctx.tx.failed(format!("游戏异常退出（{code}），看日志里的 [MC] 行"));
    }
    Ok(code)
}

/// The vanilla log4j config prints one XML event per line; turn that back into
/// readable text so the launcher log (and the GUI) stays useful.
fn clean_line(line: &str) -> Option<String> {
    let mut text = line.trim();
    if text.is_empty() {
        return None;
    }
    if let Some(i) = text.find("<![CDATA[") {
        text = &text[i + 9..];
    }
    if let Some(i) = text.find("]]>") {
        text = &text[..i];
    }
    let text = text.trim();
    if text.is_empty() || text.contains("log4j:") {
        return None;
    }
    Some(text.to_string())
}

async fn pump<R>(reader: R, tx: crate::progress::ProgressSender)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(reader).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(clean) = clean_line(&line) {
            tx.log(format!("[MC] {clean}"));
        }
    }
}

/// Pick the JVM for a version: honour an explicitly configured one only when it
/// is new enough, otherwise search the machine and finally download from Mojang.
async fn resolve_java(
    ctx: &Ctx,
    layout: &Layout,
    want: u32,
    component: Option<&str>,
    concurrency: usize,
    auto: bool,
    explicit: Option<&Path>,
) -> Result<PathBuf> {
    if let Some(p) = explicit.filter(|p| p.is_file()) {
        match crate::util::java_major(p) {
            Ok(major) if major >= want => {
                ctx.log(format!("使用指定 Java {major}: {}", p.display()));
                return Ok(p.to_path_buf());
            }
            Ok(major) => ctx.log(format!(
                "指定的 Java 是 {major}，低于所需的 {want}，忽略并自动选择：{}",
                p.display()
            )),
            Err(e) => ctx.log(format!("无法识别指定的 Java {}：{e}", p.display())),
        }
    }

    match crate::util::find_java(None, &layout.java_dir(), want) {
        Ok(found) => {
            ctx.log(format!("使用 Java {}：{}", found.major, found.path.display()));
            Ok(found.path)
        }
        Err(e) if auto => {
            ctx.log(format!("{e}；开始下载 Mojang Java {want}"));
            crate::install::ensure_java(ctx, component, want, concurrency).await
        }
        Err(e) => Err(e),
    }
}

/// Convenience: everything needed before `run`, in the right order.
pub async fn prepare_instance(
    ctx: &Ctx,
    layout: &Layout,
    instance: &Instance,
    concurrency: usize,
    java_override: Option<&Path>,
    java_auto: bool,
) -> Result<LaunchOptions> {
    // install/prepare first: the client jar is how we learn the real Java level
    // (newer version JSONs report `major_version: 0`)
    let prepared = crate::install::prepare(ctx, &instance.version_id, concurrency).await?;
    let component = prepared.version.java_component();
    let want = crate::util::required_java_major(
        prepared
            .version
            .java_version
            .as_ref()
            .map(|j| j.major_version),
        component.as_deref(),
        Some(&prepared.client_jar),
    );
    ctx.log(format!("{} 需要 Java {want}+", instance.version_id));

    let java = resolve_java(
        ctx,
        layout,
        want,
        component.as_deref(),
        concurrency,
        java_auto,
        java_override,
    )
    .await?;

    Ok(LaunchOptions {
        instance: instance.clone(),
        account_name: String::new(),
        account_uuid: String::new(),
        access_token: "0".into(),
        user_type: "legacy".into(),
        java,
        prepared,
        natives_dir: layout.instance_natives_dir(&instance.id),
        resolution: None,
        extra_jvm_args: String::new(),
    })
}

/// Do the whole pre-flight and return the command line, without spawning.
pub async fn compose(
    ctx: &Ctx,
    mut opts: LaunchOptions,
) -> Result<CommandLine> {
    // native extraction has to happen before java starts
    let natives_dir = opts.natives_dir.clone();
    let mut jar_list = Vec::new();
    std::mem::swap(&mut jar_list, &mut opts.prepared.native_jars);
    let extracted = crate::install::extract_natives(&jar_list, &natives_dir)?;
    opts.prepared.native_jars = jar_list;
    if extracted > 0 {
        ctx.log(format!("解出 {extracted} 个本地库文件"));
    }
    let cmd = build_command(&opts)?;
    Ok(cmd)
}