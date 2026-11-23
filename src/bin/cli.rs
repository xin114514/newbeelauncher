//! Command line interface: install, launch and browse without the GUI.

use anyhow::{anyhow, bail, Context, Result};
use newbeelauncher::auth::{self, Account};
use newbeelauncher::config::{self, Instance, LauncherConfig, LoaderKind};
use newbeelauncher::paths::Layout;
use newbeelauncher::progress::{Event, ProgressSender};
use newbeelauncher::Ctx;

fn usage() {
    println!(
        r#"NewBee Launcher CLI

用法:
  nbl-cli versions                         列出可安装的正式版本
  nbl-cli installed                        列出已安装的版本
  nbl-cli list                             列出实例
  nbl-cli install <mc> [--loader fabric|quilt|forge|neoforge]
                        [--loader-version <v>] [--name <实例名>]
  nbl-cli launch <实例 id|名字> [--offline <名字>]
  nbl-cli accounts                         列出账号
  nbl-cli login                            微软设备码登录（会长期保存）
  nbl-cli add-offline <名字>               添加离线账号
  nbl-cli search <关键词> [--type mod|resourcepack|shader]
                        [--mc <版本>] [--loader fabric|quilt|forge|neoforge]
  nbl-cli add <实例> <Modrinth 项目 id> [--type mod|resourcepack|shader]
  nbl-cli java <实例>                      检查该实例需要哪个 Java、实际会选哪个
  nbl-cli instances-dir [新路径]           查看/切换实例储存位置（会搬迁已有实例）
  nbl-cli delete <实例> [--yes]            删除实例并删掉它的本地文件（存档、mods…）
  nbl-cli info                             显示数据目录与设置
"#
    );
}

fn take_flag(args: &[String], name: &str) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
        if let Some(rest) = a.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
    }
    None
}

fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            skip = true;
            continue;
        }
        out.push(a.clone());
    }
    out
}

fn parse_loader(s: &str) -> LoaderKind {
    match s.to_ascii_lowercase().as_str() {
        "fabric" => LoaderKind::Fabric,
        "quilt" => LoaderKind::Quilt,
        "forge" => LoaderKind::Forge,
        "neoforge" => LoaderKind::NeoForge,
        _ => LoaderKind::Vanilla,
    }
}

fn spawn_printer(rx: std::sync::mpsc::Receiver<Event>) {
    use std::io::Write;
    std::thread::spawn(move || {
        let mut last_stage = String::new();
        let out = |s: String| {
            println!("{s}");
            let _ = std::io::stdout().flush();
        };
        while let Ok(ev) = rx.recv() {
            match ev {
                Event::Log(l) => out(l),
                Event::Stage(s) => {
                    if s != last_stage {
                        out(format!("== {s}"));
                        last_stage = s;
                    }
                }
                Event::Progress { done, total, .. } => {
                    if done == total || total == 0 {
                        eprintln!("   进度 {done}/{total}");
                    }
                }
                Event::DeviceCode {
                    user_code,
                    verification_uri,
                    message,
                } => {
                    out("──────────────────────────────".to_string());
                    out(message);
                    out(format!("代码: {user_code}"));
                    out(format!("网址: {verification_uri}"));
                    out("──────────────────────────────".to_string());
                }
                Event::Done(m) => out(format!("✔ {m}")),
                Event::Failed(m) => {
                    eprintln!("✘ {m}");
                    let _ = std::io::stderr().flush();
                }
            }
        }
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return Ok(());
    }
    let (tx, rx) = ProgressSender::channel();
    spawn_printer(rx);

    let layout = Layout::default_layout();
    layout.ensure()?;
    let mut cfg = LauncherConfig::load(&layout)?;
    // honour a custom instance store before anything resolves instance paths
    let layout = layout.with_instances_root(
        cfg.settings.instances_dir
            .as_ref()
            .map(std::path::PathBuf::from),
    );
    layout.ensure()?;
    let ctx = Ctx::new(layout.clone(), tx.clone());
    let concurrency = cfg.settings.concurrency.max(4);
    let cmd = args[0].as_str();

    match cmd {
        "java" => {
            let pos = positional(&args);
            let key = pos.get(1).ok_or_else(|| anyhow!("需要实例 id 或名字"))?;
            let inst = cfg
                .instances
                .iter()
                .find(|i| &i.id == key || &i.name == key)
                .cloned()
                .ok_or_else(|| anyhow!("找不到实例 {key}"))?;
            let version =
                newbeelauncher::mojang::load_version(&ctx, &inst.version_id).await?;
            let jar_id = version
                .client_jar_id
                .clone()
                .unwrap_or_else(|| inst.version_id.clone());
            let jar = layout.version_jar(&jar_id);
            let declared = version.java_version.as_ref().map(|j| j.major_version);
            let component = version.java_component();
            let want = newbeelauncher::util::required_java_major(
                declared,
                component.as_deref(),
                Some(&jar),
            );
            println!("实例 {}  (版本 id {})", inst.name, inst.version_id);
            println!("  JSON 声明 javaVersion.major_version = {declared:?}");
            println!("  运行时组件 = {component:?}");
            println!("  客户端 jar = {}", jar.display());
            match newbeelauncher::util::java_major_from_jar(&jar) {
                Ok(m) => println!("  jar 内 class 文件对应 Java {m}"),
                Err(e) => println!("  读不出 jar 的 class 版本: {e}"),
            }
            println!("=> 该版本需要 Java {want}+");
            let explicit = cfg.settings.java_path.clone();
            if let Some(p) = &explicit {
                println!("   设置里指定的 java: {p}");
            }
            match newbeelauncher::util::find_java(
                explicit.as_deref().map(std::path::Path::new),
                &layout.java_dir(),
                want,
            ) {
                Ok(f) => println!("=> 会使用 Java {} : {}", f.major, f.path.display()),
                Err(e) => println!(
                    "=> 没有满足条件的 Java: {e}\n   auto_download_java = {}（启动时会自动下载）",
                    cfg.settings.auto_download_java
                ),
            }
        }
        "delete" | "remove" | "rm" => {
            let pos = positional(&args);
            let key = pos.get(1).ok_or_else(|| anyhow!("需要实例 id 或名字"))?;
            let inst = cfg
                .instances
                .iter()
                .find(|i| &i.id == key || &i.name == key)
                .cloned()
                .ok_or_else(|| anyhow!("找不到实例 {key}"))?;
            let dir = layout.instance_dir(&inst.id);
            println!("实例: {}  ({})", inst.name, inst.id);
            println!(
                "游戏版本: {} / {}",
                inst.mc_version,
                inst.loader.label()
            );
            println!("将删除目录: {}", dir.display());
            if !args.iter().any(|a| a == "--yes") {
                use std::io::Write;
                print!("确认删除（含存档、mods，无法恢复）？[y/N] ");
                let _ = std::io::stdout().flush();
                let mut line = String::new();
                std::io::stdin().read_line(&mut line)?;
                if !line.trim().eq_ignore_ascii_case("y") {
                    println!("已取消");
                    return Ok(());
                }
            }
            match newbeelauncher::config::delete_instance_dir(&layout, &inst.id)? {
                Some(p) => println!("已删除 {}", p.display()),
                None => println!("目录不存在，只删记录"),
            }
            cfg.remove_instance(&inst.id);
            cfg.save(&layout)?;
            println!("已从实例列表移除（共享的 versions/libraries/assets 保留）");
        }
        "instances-dir" | "instances_dir" => {
            let pos = positional(&args);
            let current = layout.instances_dir();
            println!("当前实例目录: {}", current.display());
            if let Some(arg) = pos.get(1) {
                let new = std::path::PathBuf::from(arg);
                if new == current {
                    println!("路径没有变化");
                } else {
                    newbeelauncher::config::validate_instances_dir(&new)?;
                    let moved =
                        newbeelauncher::config::relocate_instances(&current, &new, &tx)?;
                    let default_dir = layout.root().join("instances");
                    cfg.settings.instances_dir = if new == default_dir {
                        None
                    } else {
                        Some(new.to_string_lossy().to_string())
                    };
                    cfg.save(&layout)?;
                    println!("已切换实例目录，移动 {moved} 个实例");
                    println!("新路径: {}", new.display());
                }
            }
        }
        "info" => {
            println!("数据目录: {}", layout.root().display());
            println!("配置: {}", layout.config_file().display());
            println!("并发: {}", cfg.settings.concurrency);
            println!("账号: {}", cfg.accounts.len());
            println!("实例: {}", cfg.instances.len());
        }
        "versions" => {
            let m = newbeelauncher::mojang::fetch_manifest(&ctx).await?;
            println!("最新正式版: {}", m.latest.release);
            for v in m.versions.iter().filter(|v| v.kind == "release").take(40) {
                println!("{}  {}", v.id, v.release_time);
            }
        }
        "installed" => {
            let list = newbeelauncher::install::installed_versions(&layout);
            if list.is_empty() {
                println!("(空)");
            }
            for id in list {
                let has_jar = layout.version_jar(&id).exists();
                println!("{id}  jar={}", if has_jar { "有" } else { "无" });
            }
        }
        "list" => {
            for i in &cfg.instances {
                println!(
                    "{}  [{} {} {}]  {} MB  {}",
                    i.id,
                    i.mc_version,
                    i.loader.label(),
                    i.loader_version.clone().unwrap_or_default(),
                    i.memory_mb,
                    i.name
                );
            }
        }
        "accounts" => {
            for a in &cfg.accounts {
                println!(
                    "{}{}  {}  ({})",
                    if cfg.active_account.as_deref() == Some(a.id.as_str()) {
                        "* "
                    } else {
                        "  "
                    },
                    a.name,
                    a.id,
                    config::account_kind_label(&a.kind)
                );
            }
        }
        "login" => {
            println!("开始微软设备码登录…");
            let client_id = cfg.settings.microsoft_client_id.clone();
            let profile = auth::login_device_code(&ctx.client, &client_id, &tx).await?;
            if let Some(used) = profile.used_client_id.clone() {
                if !used.is_empty() && cfg.settings.microsoft_client_id != used {
                    println!("已记住可用的 Microsoft 应用 ID: {used}");
                    cfg.settings.microsoft_client_id = used;
                }
            }
            let account = profile.into_account();
            println!("登录成功: {} ({})", account.name, account.id);
            cfg.upsert_account(account.clone());
            cfg.activate(&account.id);
            cfg.save(&layout)?;
            println!("已保存到 {}", layout.config_file().display());
        }
        "add-offline" => {
            let pos = positional(&args);
            let name = pos.get(1).ok_or_else(|| anyhow!("需要用户名"))?;
            if !config::is_offline_name_valid(name) {
                bail!("离线用户名需要 3–16 位字母、数字或下划线");
            }
            let a = Account::offline(name);
            cfg.upsert_account(a.clone());
            cfg.activate(&a.id);
            cfg.save(&layout)?;
            println!("已添加离线账号 {name} ({})", a.id);
        }
        "install" => {
            let pos = positional(&args);
            let mc = pos
                .get(1)
                .ok_or_else(|| anyhow!("需要游戏版本，例如 1.21.1"))?
                .as_str();
            let loader = take_flag(&args, "--loader")
                .map(|s| parse_loader(&s))
                .unwrap_or(LoaderKind::Vanilla);
            let loader_version = take_flag(&args, "--loader-version");
            let name = take_flag(&args, "--name").unwrap_or_else(|| {
                if loader == LoaderKind::Vanilla {
                    format!("{mc}-原版")
                } else {
                    format!("{mc}-{}", loader.label())
                }
            });

            println!("安装 {mc} / {} …", loader.label());
            let mut inst = Instance::new(
                name.as_str(),
                mc,
                loader,
                loader_version.clone(),
                mc,
            );
            inst.memory_mb = cfg.settings.default_memory_mb;
            layout.ensure_instance(&inst.id)?;
            let vid = newbeelauncher::loaders::install(
                &ctx,
                mc,
                loader,
                loader_version.as_deref(),
                concurrency,
                None,
            )
            .await?;
            inst.version_id = vid.clone();
            cfg.add_instance(inst.clone());
            cfg.save(&layout)?;
            println!("完成: 实例 {} ({}) 版本 id = {vid}", inst.name, inst.id);
            println!("启动: nbl-cli launch {}", inst.id);
        }
        "launch" => {
            let pos = positional(&args);
            let key = pos.get(1).ok_or_else(|| anyhow!("需要实例 id 或名字"))?;
            let inst = cfg
                .instances
                .iter()
                .find(|i| &i.id == key || &i.name == key)
                .cloned()
                .ok_or_else(|| anyhow!("找不到实例 {key}"))?;
            let offline = take_flag(&args, "--offline");
            let account = match offline {
                Some(name) => Account::offline(&name),
                None => cfg
                    .active()
                    .cloned()
                    .ok_or_else(|| anyhow!("没有账号，用 --offline <名字> 或先 nbl-cli login"))?,
            };
            let token = auth::ensure_session(
                &ctx.client,
                &cfg.settings.microsoft_client_id,
                &account,
                &tx,
            )
            .await?;
            let java = cfg.settings.java_path.as_deref().map(std::path::Path::new);
            let mut opts = newbeelauncher::launch::prepare_instance(
                &ctx,
                &layout,
                &inst,
                concurrency,
                java,
                cfg.settings.auto_download_java,
            )
            .await?;
            opts.account_name = account.name.clone();
            opts.account_uuid = account.id.clone();
            opts.access_token = token;
            opts.user_type = if account.is_offline() {
                "legacy".into()
            } else {
                "msa".into()
            };
            opts.extra_jvm_args = cfg.settings.extra_jvm_args.clone();
            let command = newbeelauncher::launch::compose(&ctx, opts).await?;
            println!(
                "java: {} ({} 个参数，完整命令行见 game/launcher-command.txt)",
                command.program.display(),
                command.args.len()
            );
            let code = newbeelauncher::launch::run(&ctx, command).await?;
            std::process::exit(code);
        }
        "search" => {
            let pos = positional(&args);
            let query = pos.get(1).map(|s| s.as_str()).unwrap_or("");
            let ty = take_flag(&args, "--type").unwrap_or_else(|| "mod".into());
            let mc = take_flag(&args, "--mc");
            let loader = take_flag(&args, "--loader").map(|s| parse_loader(&s));
            let cats: Vec<&str> = match (&loader, ty.as_str()) {
                (Some(k), "mod") if *k != LoaderKind::Vanilla => vec![k.as_str()],
                _ => vec![],
            };
            println!(
                "facets: {}",
                newbeelauncher::modrinth::facets(&ty, mc.as_deref(), &cats)
            );
            let res = newbeelauncher::modrinth::search(
                &ctx,
                query,
                &ty,
                mc.as_deref(),
                &cats,
                20,
                0,
            )
            .await?;
            println!("共 {} 个结果", res.total_hits);
            for h in res.hits {
                println!(
                    "{}  {}  ({} 下载)\n    {}",
                    h.project_id,
                    h.title,
                    newbeelauncher::util::human_bytes(h.downloads),
                    h.description
                );
            }
        }
        "add" => {
            let pos = positional(&args);
            let key = pos.get(1).ok_or_else(|| anyhow!("需要实例 id"))?;
            let pid = pos.get(2).ok_or_else(|| anyhow!("需要 Modrinth 项目 id"))?;
            let ty = take_flag(&args, "--type").unwrap_or_else(|| "mod".into());
            let inst = cfg
                .instances
                .iter()
                .find(|i| &i.id == key || &i.name == key)
                .cloned()
                .ok_or_else(|| anyhow!("找不到实例 {key}"))?;
            let loader = if ty == "mod" {
                Some(inst.loader.as_str())
            } else {
                None
            };
            let files = newbeelauncher::modrinth::install_by_project(
                &ctx,
                &layout,
                &inst.id,
                &ty,
                pid,
                &inst.mc_version,
                loader,
                concurrency,
            )
            .await?;
            for f in files {
                println!("{}", f.display());
            }
        }
        other => {
            eprintln!("未知命令: {other}");
            usage();
            std::process::exit(2);
        }
    }
    let _ = cfg.save(&layout).with_context(|| "保存配置");
    Ok(())
}