//! egui front-end.

use crate::auth::{self, Account};
use crate::config::{self, Instance, LauncherConfig, LoaderKind};
use crate::modrinth::{self, SearchHit};
use crate::paths::Layout;
use crate::progress::{Event, ProgressSender};
use crate::Ctx;
use anyhow::Result;
use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Overview,
    Content,
    Log,
    Settings,
}

enum DataMsg {
    /// account + the client id that actually worked (may differ from settings)
    Account(Account, Option<String>),
    Versions(Vec<String>),
    Loaders(Vec<String>),
    Results(Vec<SearchHit>, u64),
    Content(Vec<String>),
    Created(Instance),
    /// new instance store + how many instance folders were moved
    InstancesMoved(PathBuf, usize),
    Error(String),
    /// The stored Microsoft refresh token is permanently invalid and needs a new device-code login.
    Reauthenticate(String),
    /// Installation completed; refresh the installed list and show a confirmation dialog.
    InstallFinished { message: String, items: Vec<String> },
    DependencyPlan(crate::modrinth::DependencyPlan),
}

enum Action {
    Start(String),
    Delete(String),
    ConfirmDelete(String),
    OpenDir(String),
    LoginMs,
    AddOffline,
    CreateInstance,
    SearchMods,
    InspectDependencies(String),
    InstallWithDependencies { target: String, dependencies: Vec<String> },
    RemoveContent(String),
    RefreshContent,
    SaveSettings,
    LoadLoaderVersions,
    ApplyInstancesDir,
    OpenInstancesDir,
}

pub struct NblApp {
    layout: Layout,
    cfg: LauncherConfig,
    client: reqwest::Client,
    tx: ProgressSender,
    rx: Receiver<Event>,
    datatx: Sender<DataMsg>,
    datarx: Receiver<DataMsg>,
    rt: tokio::runtime::Runtime,

    log: Vec<String>,
    stage: String,
    progress: Option<(u64, u64)>,
    busy: bool,
    tab: Tab,
    selected: Option<String>,

    versions: Vec<String>,
    loader_versions: Vec<String>,
    results: Vec<SearchHit>,
    total_hits: u64,
    content: Vec<String>,

    show_login: bool,
    show_new: bool,
    offline_name: String,
    device: Option<(String, String, String)>,

    form_name: String,
    form_mc: String,
    form_loader: LoaderKind,
    form_loader_ver: String,

    query: String,
    content_type: String,
    /// type of the currently listed search results (so "安装" lands in the right folder)
    results_type: String,
    manifest_loaded: bool,
    cfg_changed: bool,
    /// text field for the instance store path
    instances_dir_input: String,
    /// instance waiting for a delete confirmation: (id, name, folder)
    confirm_delete: Option<(String, String, PathBuf)>,
    install_notice: Option<String>,
    dependency_plan: Option<crate::modrinth::DependencyPlan>,
    dependency_warning: bool,
}

impl NblApp {
    fn new(cc: &eframe::CreationContext<'_>, layout: Layout) -> Self {
        setup_fonts(&cc.egui_ctx);
        let (tx, rx) = ProgressSender::channel();
        let (datatx, datarx) = std::sync::mpsc::channel();
        // A failed read must never be followed by a silent overwrite of the file on
        // disk (that would wipe every account and instance record), so keep a copy
        // and say so in the log.
        let mut load_error: Option<String> = None;
        let cfg = match LauncherConfig::load(&layout) {
            Ok(c) => c,
            Err(e) => {
                let src = layout.config_file();
                let backup = layout
                    .root()
                    .join(format!("config.json.corrupt-{}", crate::util::now_ts()));
                if src.exists() {
                    let _ = std::fs::copy(&src, &backup);
                }
                load_error = Some(format!(
                    "读取配置失败：{e}；原文件已备份为 {}",
                    backup.display()
                ));
                LauncherConfig::default()
            }
        };
        // the instance store may live somewhere else entirely; wire it up before
        // anything touches instance paths
        let layout = layout.with_instances_root(
            cfg.settings.instances_dir.as_ref().map(PathBuf::from),
        );
        let _ = layout.ensure();
        let instances_dir_input = cfg.settings.instances_dir.clone().unwrap_or_default();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("tokio runtime");

        let mut app = Self {
            layout: layout.clone(),
            selected: cfg.instances.first().map(|i| i.id.clone()),
            cfg,
            client: crate::net::client(),
            tx,
            rx,
            datatx,
            datarx,
            rt,
            log: Vec::new(),
            stage: String::new(),
            progress: None,
            busy: false,
            tab: Tab::Overview,
            versions: Vec::new(),
            loader_versions: Vec::new(),
            results: Vec::new(),
            total_hits: 0,
            content: Vec::new(),
            show_login: false,
            show_new: false,
            offline_name: String::new(),
            device: None,
            form_name: "新实例".into(),
            form_mc: "1.21.1".into(),
            form_loader: LoaderKind::Vanilla,
            form_loader_ver: String::new(),
            query: String::new(),
            content_type: "mod".into(),
            results_type: String::new(),
            manifest_loaded: false,
            cfg_changed: false,
            instances_dir_input,
            confirm_delete: None,
            install_notice: None,
            dependency_plan: None,
            dependency_warning: false,
        };
        app.log(format!("数据目录: {}", layout.root().display()));
        if let Some(e) = load_error {
            app.log(e);
        }
        app.load_manifest();
        if let Some(id) = app.selected.clone() {
            app.refresh_content(&id);
        }
        app
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            client: self.client.clone(),
            layout: self.layout.clone(),
            tx: self.tx.clone(),
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        self.log.push(line.into());
        if self.log.len() > 4000 {
            let drain = self.log.len() - 4000;
            self.log.drain(0..drain);
        }
    }

    fn selected_instance(&self) -> Option<&Instance> {
        self.selected
            .as_deref()
            .and_then(|id| self.cfg.instance(id))
    }

    fn load_manifest(&mut self) {
        let ctx = self.ctx();
        let datatx = self.datatx.clone();
        self.rt.spawn(async move {
            match crate::mojang::fetch_manifest(&ctx).await {
                Ok(m) => {
                    let releases: Vec<String> = m
                        .versions
                        .iter()
                        .filter(|v| v.kind == "release")
                        .map(|v| v.id.clone())
                        .collect();
                    let _ = datatx.send(DataMsg::Versions(releases));
                }
                Err(e) => {
                    let _ = datatx.send(DataMsg::Error(format!("版本清单: {e}")));
                }
            }
        });
    }

    fn refresh_content(&mut self, id: &str) {
        let Some(inst) = self.cfg.instance(id).cloned() else {
            return;
        };
        let layout = self.layout.clone();
        let datatx = self.datatx.clone();
        let rt = &self.rt;
        rt.spawn(async move {
            let mut out = config::list_dir(&layout.instance_mods_dir(&inst.id));
            out.extend(
                config::list_dir(&layout.instance_resourcepacks_dir(&inst.id))
                    .into_iter()
                    .map(|f| format!("[资源包] {f}")),
            );
            out.extend(
                config::list_dir(&layout.instance_shaderpacks_dir(&inst.id))
                    .into_iter()
                    .map(|f| format!("[光影] {f}")),
            );
            let _ = datatx.send(DataMsg::Content(out));
        });
    }

    fn handle(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::LoadLoaderVersions => {
                let mc = self.form_mc.clone();
                let kind = self.form_loader;
                if kind == LoaderKind::Vanilla {
                    self.loader_versions.clear();
                    return;
                }
                let ctx = self.ctx();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.rt.spawn(async move {
                    match crate::loaders::list_versions(&ctx, &mc, kind).await {
                        Ok(list) => {
                            let v: Vec<String> = list.into_iter().map(|l| l.version).collect();
                            let _ = datatx.send(DataMsg::Loaders(v));
                        }
                        Err(e) => {
                            let _ = datatx.send(DataMsg::Error(format!("加载器版本: {e}")));
                        }
                    }
                });
            }
            Action::LoginMs => {
                let ctx = self.ctx();
                let datatx = self.datatx.clone();
                let client_id = self.cfg.settings.microsoft_client_id.clone();
                self.busy = true;
                self.log("开始微软登录…");
                self.rt.spawn(async move {
                    match auth::login_device_code(&ctx.client, &client_id, &ctx.tx).await {
                        Ok(p) => {
                            let used = p.used_client_id.clone();
                            let _ = datatx.send(DataMsg::Account(p.into_account(), used));
                            ctx.tx.done("微软登录成功");
                        }
                        Err(e) => ctx.tx.failed(format!("微软登录失败: {e}")),
                    }
                });
            }
            Action::AddOffline => {
                let name = self.offline_name.trim().to_string();
                if !config::is_offline_name_valid(&name) {
                    self.log("离线用户名需要 3–16 位字母、数字或下划线");
                    return;
                }
                let acct = Account::offline(&name);
                self.log(format!("已添加离线账号 {name}"));
                self.cfg.upsert_account(acct.clone());
                self.cfg.activate(&acct.id);
                self.persist();
                self.show_login = false;
            }
            Action::CreateInstance => {
                let name = self.form_name.trim().to_string();
                if name.is_empty() {
                    self.log("实例名不能为空");
                    return;
                }
                if self.cfg.name_taken(&name) {
                    self.log(format!("实例名 {name} 已存在"));
                    return;
                }
                let mc = self.form_mc.clone();
                let kind = self.form_loader;
                let lv = self.form_loader_ver.trim().to_string();
                let lv = if lv.is_empty() { None } else { Some(lv) };
                let settings = self.cfg.settings.clone();
                let layout = self.layout.clone();
                let ctx = self.ctx();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.show_new = false;
                self.log(format!(
                    "创建实例 {name}: {mc} / {} {}",
                    kind.label(),
                    lv.clone().unwrap_or_default()
                ));
                self.rt.spawn(async move {
                    let res: Result<Instance> = async {
                        let mut inst = Instance::new(&name, &mc, kind, lv.clone(), "");
                        inst.memory_mb = settings.default_memory_mb;
                        layout.ensure_instance(&inst.id)?;
                        let vid = crate::loaders::install(
                            &ctx,
                            &mc,
                            kind,
                            lv.as_deref(),
                            settings.concurrency,
                            None,
                        )
                        .await?;
                        inst.version_id = vid;
                        Ok(inst)
                    }
                    .await;
                    match res {
                        Ok(inst) => {
                            let _ = datatx.send(DataMsg::Created(inst));
                            ctx.tx.done("实例创建完成");
                        }
                        Err(e) => ctx.tx.failed(format!("创建实例失败: {e}")),
                    }
                });
            }
            Action::Start(id) => {
                let Some(inst) = self.cfg.instance(&id).cloned() else {
                    return;
                };
                let Some(acct) = self.cfg.active().cloned() else {
                    self.log("请先添加账号");
                    self.show_login = true;
                    return;
                };
                let settings = self.cfg.settings.clone();
                let layout = self.layout.clone();
                let ctx = self.ctx();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.stage = "启动游戏".into();
                self.rt.spawn(async move {
                    let out: Result<()> = async {
                        let token = auth::ensure_session(
                            &ctx.client,
                            &settings.microsoft_client_id,
                            &acct,
                            &ctx.tx,
                        )
                        .await?;
                        let java = settings.java_path.as_deref().map(std::path::Path::new);
                        let mut opts = crate::launch::prepare_instance(
                            &ctx,
                            &layout,
                            &inst,
                            settings.concurrency,
                            java,
                            settings.auto_download_java,
                        )
                        .await?;
                        opts.account_name = acct.name.clone();
                        opts.account_uuid = acct.id.clone();
                        opts.access_token = token;
                        opts.user_type = if acct.is_offline() {
                            "legacy".to_string()
                        } else {
                            "msa".to_string()
                        };
                        opts.extra_jvm_args = settings.extra_jvm_args.clone();
                        let cmd = crate::launch::compose(&ctx, opts).await?;
                        crate::launch::run(&ctx, cmd).await?;
                        Ok(())
                    }
                    .await;
                    if let Err(e) = out {
                        if auth::is_reauthentication_required(&e) {
                            let _ = datatx.send(DataMsg::Reauthenticate(format!(
                                "微软账号 {} 的登录令牌已失效（Microsoft 返回 invalid_grant/AADSTS7000012）。请重新登录。",
                                acct.name
                            )));
                        }
                        ctx.tx.failed(format!("启动失败: {e}"));
                    }
                });
            }
            Action::Delete(id) => {
                // wiping the folder is destructive, so always confirm first
                if let Some(inst) = self.cfg.instance(&id).cloned() {
                    self.confirm_delete =
                        Some((id, inst.name, self.layout.instance_dir(&inst.id)));
                }
            }
            Action::ConfirmDelete(id) => {
                let Some(inst) = self.cfg.instance(&id).cloned() else {
                    return;
                };
                let dir = self.layout.instance_dir(&id);
                match config::delete_instance_dir(&self.layout, &id) {
                    Ok(Some(path)) => self.log(format!("已删除目录 {}", path.display())),
                    Ok(None) => self.log(format!("目录 {} 不存在，只删记录", dir.display())),
                    Err(e) => {
                        self.log(format!("删除失败：{e}"));
                        return;
                    }
                }
                self.cfg.remove_instance(&id);
                self.persist();
                if self.selected.as_deref() == Some(id.as_str()) {
                    self.selected = self.cfg.instances.first().map(|i| i.id.clone());
                    self.content.clear();
                    if let Some(next) = self.selected.clone() {
                        self.refresh_content(&next);
                    }
                }
                self.log(format!(
                    "已删除实例「{}」及其文件（共享的 versions/libraries/assets 仍在 shared/ 里）",
                    inst.name
                ));
            }
            Action::OpenDir(id) => {
                let dir = self.layout.instance_game_dir(&id);
                let _ = std::fs::create_dir_all(&dir);
                open_in_file_manager(&dir);
            }
            Action::SearchMods => {
                let Some(inst) = self.selected_instance().cloned() else {
                    self.log("先在左侧选择一个实例");
                    return;
                };
                let q = self.query.clone();
                let ty = self.content_type.clone();
                self.results_type = ty.clone();
                let mc = inst.mc_version.clone();
                let loader = inst.loader.as_str().to_string();
                let is_mod = ty == "mod";
                let ctx = self.ctx();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.stage = format!("搜索 {ty}");
                self.rt.spawn(async move {
                    let cats: Vec<&str> = if is_mod && loader != "vanilla" {
                        vec![loader.as_str()]
                    } else {
                        vec![]
                    };
                    match modrinth::search(&ctx, &q, &ty, Some(&mc), &cats, 30, 0).await {
                        Ok(r) => {
                            let _ = datatx.send(DataMsg::Results(r.hits, r.total_hits));
                        }
                        Err(e) => {
                            let _ = datatx.send(DataMsg::Error(format!("搜索失败: {e}")));
                        }
                    }
                });
            }
            Action::InspectDependencies(pid) => {
                let Some(inst) = self.selected_instance().cloned() else { return; };
                let ctx = self.ctx();
                let layout = self.layout.clone();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.rt.spawn(async move {
                    let loader = if inst.loader == LoaderKind::Vanilla { None } else { Some(inst.loader.as_str()) };
                    match modrinth::dependency_plan(&ctx, &layout, &inst.id, &pid, &inst.mc_version, loader).await {
                        Ok(plan) => { let _ = datatx.send(DataMsg::DependencyPlan(plan)); }
                        Err(e) => { let _ = datatx.send(DataMsg::Error(format!("解析 Mod 前置失败: {e}"))); }
                    }
                });
            }
            Action::InstallWithDependencies { target, dependencies } => {
                let Some(inst) = self.selected_instance().cloned() else { return; };
                let layout = self.layout.clone();
                let settings = self.cfg.settings.clone();
                let ctx = self.ctx();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.rt.spawn(async move {
                    let loader = if inst.loader == LoaderKind::Vanilla { None } else { Some(inst.loader.as_str()) };
                    let mut count = 0usize;
                    for dep in dependencies {
                        if let Ok(files) = modrinth::install_by_project(&ctx, &layout, &inst.id, "mod", &dep, &inst.mc_version, loader, settings.concurrency).await { count += files.len(); }
                    }
                    match modrinth::install_by_project(&ctx, &layout, &inst.id, "mod", &target, &inst.mc_version, loader, settings.concurrency).await {
                        Ok(files) => {
                            let mut items = config::list_dir(&layout.instance_mods_dir(&inst.id));
                            items.extend(config::list_dir(&layout.instance_resourcepacks_dir(&inst.id)).into_iter().map(|f| format!("[资源包] {f}")));
                            items.extend(config::list_dir(&layout.instance_shaderpacks_dir(&inst.id)).into_iter().map(|f| format!("[光影] {f}")));
                            let total = count + files.len();
                            let _ = datatx.send(DataMsg::InstallFinished { message: format!("安装完成，共下载 {total} 个文件"), items });
                            ctx.tx.done(format!("安装完成，共下载 {total} 个文件"));
                        }
                        Err(e) => ctx.tx.failed(format!("安装失败: {e}")),
                    }
                });
            }
            Action::RemoveContent(item) => {
                let Some(inst) = self.selected_instance().cloned() else {
                    return;
                };
                let layout = self.layout.clone();
                let (dir, name) = if let Some(n) = item.strip_prefix("[资源包] ") {
                    (layout.instance_resourcepacks_dir(&inst.id), n.to_string())
                } else if let Some(n) = item.strip_prefix("[光影] ") {
                    (layout.instance_shaderpacks_dir(&inst.id), n.to_string())
                } else {
                    (layout.instance_mods_dir(&inst.id), item.clone())
                };
                let name_for_log = name.clone();
                match std::fs::remove_file(dir.join(&name)) {
                    Ok(()) => self.log(format!("已删除 {name_for_log}")),
                    Err(e) => self.log(format!("删除失败: {e}")),
                }
                self.refresh_content(&inst.id);
            }
            Action::RefreshContent => {
                if let Some(id) = self.selected.clone() {
                    self.refresh_content(&id);
                }
            }
            Action::SaveSettings => {
                self.persist();
                self.log("设置已保存");
            }
            Action::ApplyInstancesDir => {
                let raw = self.instances_dir_input.trim().to_string();
                let old = self.layout.instances_dir();
                let new = if raw.is_empty() {
                    self.layout.root().join("instances")
                } else {
                    PathBuf::from(&raw)
                };
                if new == old {
                    self.log("实例目录没有变化");
                    return;
                }
                if let Err(e) = config::validate_instances_dir(&new) {
                    self.log(format!("实例目录不可用：{e}"));
                    return;
                }
                let tx = self.tx.clone();
                let datatx = self.datatx.clone();
                self.busy = true;
                self.stage = "迁移实例".into();
                self.log(format!("切换实例目录 → {}", new.display()));
                self.rt.spawn(async move {
                    let target = new.clone();
                    let task = {
                        let old = old.clone();
                        let new = new.clone();
                        let tx = tx.clone();
                        move || config::relocate_instances(&old, &new, &tx)
                    };
                    match tokio::task::spawn_blocking(task).await {
                        Ok(Ok(n)) => {
                            let _ = datatx.send(DataMsg::InstancesMoved(target, n));
                            tx.done(format!("实例目录已切换（移动 {n} 个实例）"));
                        }
                        Ok(Err(e)) => tx.failed(format!("切换实例目录失败：{e}")),
                        Err(e) => tx.failed(format!("迁移任务失败：{e}")),
                    }
                });
            }
            Action::OpenInstancesDir => {
                let dir = self.layout.instances_dir();
                let _ = std::fs::create_dir_all(&dir);
                open_in_file_manager(&dir);
            }
        }
        ctx.request_repaint();
    }

    fn persist(&mut self) {
        if let Err(e) = self.cfg.save(&self.layout) {
            self.log(format!("保存配置失败: {e}"));
        }
    }

    fn poll_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            match ev {
                Event::Log(l) => self.log(l),
                Event::Stage(s) => {
                    self.stage = s.clone();
                    self.log(format!("== {s}"));
                }
                Event::Progress { done, total, .. } => self.progress = Some((done, total)),
                Event::DeviceCode {
                    user_code,
                    verification_uri,
                    message,
                } => {
                    self.device = Some((user_code, verification_uri, message));
                }
                Event::Done(msg) => {
                    self.busy = false;
                    self.progress = None;
                    self.log(format!("✔ {msg}"));
                }
                Event::Failed(msg) => {
                    self.busy = false;
                    self.progress = None;
                    self.log(format!("✘ {msg}"));
                }
            }
        }
        while let Ok(msg) = self.datarx.try_recv() {
            match msg {
                DataMsg::Account(a, used_client_id) => {
                    if let Some(cid) = used_client_id {
                        if !cid.is_empty() && self.cfg.settings.microsoft_client_id != cid {
                            self.log(format!("已记住可用的 Microsoft 应用 ID: {cid}"));
                            self.cfg.settings.microsoft_client_id = cid;
                        }
                    }
                    self.cfg.upsert_account(a.clone());
                    self.cfg.activate(&a.id);
                    self.persist();
                    self.log(format!("已保存账号 {}", a.name));
                    self.device = None;
                    self.busy = false;
                }
                DataMsg::Versions(v) => {
                    if self.versions.is_empty() {
                        if let Some(first) = v.first() {
                            self.form_mc = first.clone();
                        }
                    }
                    self.versions = v;
                    self.manifest_loaded = true;
                }
                DataMsg::Loaders(v) => {
                    self.form_loader_ver = v.first().cloned().unwrap_or_default();
                    self.loader_versions = v;
                    self.busy = false;
                }
                DataMsg::Results(hits, total) => {
                    self.results = hits;
                    self.total_hits = total;
                    self.busy = false;
                    self.progress = None;
                }
                DataMsg::Content(list) => {
                    self.content = list;
                    self.busy = false;
                }
                DataMsg::Created(inst) => {
                    self.selected = Some(inst.id.clone());
                    self.cfg.add_instance(inst);
                    self.persist();
                    self.busy = false;
                }
                DataMsg::InstancesMoved(dir, moved) => {
                    let default_dir = self.layout.root().join("instances");
                    self.cfg.settings.instances_dir = if dir == default_dir {
                        None
                    } else {
                        Some(dir.to_string_lossy().to_string())
                    };
                    self.layout = self.layout.clone().with_instances_root(
                        self.cfg.settings.instances_dir.as_ref().map(PathBuf::from),
                    );
                    self.instances_dir_input =
                        self.cfg.settings.instances_dir.clone().unwrap_or_default();
                    let _ = self.layout.ensure();
                    self.persist();
                    self.log(format!(
                        "实例目录现在是 {}（移动 {moved} 项）",
                        self.layout.instances_dir().display()
                    ));
                    self.busy = false;
                    self.progress = None;
                    if let Some(id) = self.selected.clone() {
                        self.refresh_content(&id);
                    }
                }
                DataMsg::Error(e) => {
                    self.log(format!("✘ {e}"));
                    self.busy = false;
                }
                DataMsg::Reauthenticate(message) => {
                    self.log(message);
                    self.show_login = true;
                    self.busy = false;
                }
                DataMsg::InstallFinished { message, items } => {
                    self.content = items;
                    self.install_notice = Some(message.clone());
                    self.log(message);
                    self.busy = false;
                    self.progress = None;
                }
                DataMsg::DependencyPlan(plan) => {
                    self.busy = false;
                    self.dependency_plan = Some(plan);
                    self.dependency_warning = false;
                }
            }
        }
    }
}

impl eframe::App for NblApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_events();
        ctx.request_repaint_after(Duration::from_millis(120));

        let mut pending: Option<Action> = None;

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("NewBee Launcher");
                ui.separator();
                let active_name = self
                    .cfg
                    .active()
                    .map(|a| format!("{} ({})", a.name, config::account_kind_label(&a.kind)))
                    .unwrap_or_else(|| "未登录".into());
                egui::ComboBox::from_label("账号")
                    .selected_text(active_name)
                    .show_ui(ui, |ui| {
                        let accounts = self.cfg.accounts.clone();
                        for a in accounts {
                            let label = format!("{} ({})", a.name, config::account_kind_label(&a.kind));
                            if ui
                                .selectable_label(
                                    self.cfg.active_account.as_deref() == Some(a.id.as_str()),
                                    label,
                                )
                                .clicked()
                            {
                                self.cfg.activate(&a.id);
                                self.persist();
                            }
                        }
                    });
                if ui.button("添加账号").clicked() {
                    self.show_login = true;
                }
                ui.separator();
                let can_start = self.selected_instance().is_some() && !self.busy;
                if ui
                    .add_enabled(can_start, egui::Button::new("▶ 启动游戏"))
                    .clicked()
                {
                    if let Some(id) = self.selected.clone() {
                        pending = Some(Action::Start(id));
                    }
                }
                if self.busy {
                    ui.spinner();
                    if let Some((done, total)) = self.progress {
                        if total > 0 {
                            let frac = done as f32 / total as f32;
                            ui.add(
                                egui::ProgressBar::new(frac)
                                    .desired_width(160.0)
                                    .show_percentage(),
                            );
                        }
                    }
                }
            });
            if !self.stage.is_empty() {
                ui.horizontal(|ui| {
                    ui.weak(format!("阶段: {}", self.stage));
                });
            }
        });

        egui::SidePanel::left("instances")
            .resizable(true)
            .default_width(260.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("实例");
                    if ui.button("+ 新建").clicked() {
                        self.show_new = true;
                        self.loader_versions.clear();
                        if self.form_mc.is_empty() {
                            self.form_mc = self.versions.first().cloned().unwrap_or_default();
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let instances = self.cfg.instances.clone();
                    if instances.is_empty() {
                        ui.weak("还没有实例，点「+ 新建」开始。");
                    }
                    for inst in instances {
                        let selected = self.selected.as_deref() == Some(inst.id.as_str());
                        let title = format!(
                            "{}  ·  {}\n{} {}",
                            inst.name,
                            inst.mc_version,
                            inst.loader.label(),
                            inst.loader_version.clone().unwrap_or_default()
                        );
                        if ui.selectable_label(selected, title).clicked() {
                            self.selected = Some(inst.id.clone());
                            self.refresh_content(&inst.id);
                        }
                    }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Overview, "概览");
                ui.selectable_value(&mut self.tab, Tab::Content, "内容");
                ui.selectable_value(&mut self.tab, Tab::Log, "日志");
                ui.selectable_value(&mut self.tab, Tab::Settings, "设置");
            });
            ui.separator();
            match self.tab {
                Tab::Overview => {
                    if let Some(inst) = self.selected_instance().cloned() {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.heading(&inst.name);
                            ui.label(format!("游戏版本: {}", inst.mc_version));
                            ui.label(format!(
                                "加载器: {} {}",
                                inst.loader.label(),
                                inst.loader_version.clone().unwrap_or_default()
                            ));
                            ui.label(format!("版本 id: {}", inst.version_id));
                            ui.label(format!(
                                "游戏目录: {}",
                                self.layout.instance_game_dir(&inst.id).display()
                            ));
                            let mut mem = inst.memory_mb;
                            ui.add(
                                egui::Slider::new(&mut mem, 1024..=16384).text("内存 MB"),
                            );
                            if mem != inst.memory_mb {
                                if let Some(slot) = self.cfg.instance_mut(&inst.id) {
                                    slot.memory_mb = mem;
                                }
                                self.cfg_changed = true;
                            }
                            if ui.button("打开实例目录").clicked() {
                                pending = Some(Action::OpenDir(inst.id.clone()));
                            }
                            if ui.button("刷新已安装内容").clicked() {
                                pending = Some(Action::RefreshContent);
                            }
                            if ui.button("删除实例…").clicked() {
                                pending = Some(Action::Delete(inst.id.clone()));
                            }
                        });
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            if ui.button("▶ 启动").clicked() {
                                pending = Some(Action::Start(inst.id.clone()));
                            }
                            if ui.button("打开 mods").clicked() {
                                pending = Some(Action::OpenDir(inst.id.clone()));
                            }
                        });
                    } else {
                        ui.centered_and_justified(|ui| {
                            ui.label("左侧选择或新建一个实例");
                        });
                    }
                }
                Tab::Content => {
                    ui.horizontal(|ui| {
                        ui.label("类型");
                        egui::ComboBox::from_label("内容类型")
                            .selected_text(match self.content_type.as_str() {
                                "resourcepack" => "资源包",
                                "shader" => "光影",
                                _ => "模组",
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut self.content_type,
                                    "mod".to_string(),
                                    "模组",
                                );
                                ui.selectable_value(
                                    &mut self.content_type,
                                    "resourcepack".to_string(),
                                    "资源包",
                                );
                                ui.selectable_value(
                                    &mut self.content_type,
                                    "shader".to_string(),
                                    "光影",
                                );
                            });
                        ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text("搜索 Modrinth（留空看热门）")
                                .desired_width(280.0),
                        );
                        if ui.button("搜索").clicked() {
                            pending = Some(Action::SearchMods);
                        }
                        if ui.button("刷新已装").clicked() {
                            pending = Some(Action::RefreshContent);
                        }
                    });
                    if let Some(inst) = self.selected_instance() {
                        ui.weak(format!(
                            "筛选: {} · {} · 加载器 {}",
                            inst.mc_version,
                            if self.content_type == "mod" {
                                "模组"
                            } else if self.content_type == "resourcepack" {
                                "资源包"
                            } else {
                                "光影"
                            },
                            if inst.loader == LoaderKind::Vanilla {
                                "无".to_string()
                            } else {
                                inst.loader.label().to_string()
                            }
                        ));
                    }
                    ui.separator();
                    if self.total_hits > 0 {
                        ui.weak(format!("共 {} 个结果", self.total_hits));
                    }
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .show(ui, |ui| {
                            let hits = self.results.clone();
                            for hit in hits {
                                egui::Frame::group(ui.style()).show(ui, |ui| {
                                    ui.horizontal(|ui| {
                                        ui.strong(&hit.title);
                                        ui.weak(format!(
                                            "{} · 下载 {}",
                                            hit.author,
                                            crate::util::human_bytes(hit.downloads)
                                        ));
                                        let install = ui.add_enabled(
                                            !self.busy,
                                            egui::Button::new("安装").min_size(egui::vec2(72.0, 28.0)),
                                        );
                                        if install.clicked() {
                                            pending = Some(Action::InspectDependencies(
                                                hit.project_id.clone(),
                                            ));
                                        }
                                    });
                                    ui.label(&hit.description);
                                    if !hit.categories.is_empty() {
                                        ui.weak(hit.categories.join(" · "));
                                    }
                                });
                            }
                        });
                    ui.separator();
                    ui.strong(format!("已安装 ({} 项)", self.content.len()));
                    egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                        for f in self.content.clone() {
                            ui.horizontal(|ui| {
                                if ui.small_button("✖").clicked() {
                                    pending = Some(Action::RemoveContent(f.clone()));
                                }
                                ui.label(&f);
                            });
                        }
                    });
                }
                Tab::Log => {
                    egui::ScrollArea::vertical()
                        .stick_to_bottom(true)
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            for line in &self.log {
                                ui.monospace(line);
                            }
                        });
                }
                Tab::Settings => {
                    ui.heading("设置");
                    egui::Grid::new("settings").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label("Java 路径（留空自动查找）");
                        let mut java = self.cfg.settings.java_path.clone().unwrap_or_default();
                        if ui.text_edit_singleline(&mut java).changed() {
                            self.cfg.settings.java_path =
                                if java.trim().is_empty() { None } else { Some(java) };
                        }
                        ui.end_row();

                        ui.label("默认内存 MB");
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.settings.default_memory_mb)
                                .range(1024..=32768),
                        );
                        ui.end_row();

                        ui.label("下载并发");
                        ui.add(
                            egui::DragValue::new(&mut self.cfg.settings.concurrency)
                                .range(1..=64),
                        );
                        ui.end_row();

                        ui.label("微软 Client ID");
                        ui.text_edit_singleline(&mut self.cfg.settings.microsoft_client_id);
                        ui.end_row();

                        ui.label("额外 JVM 参数");
                        ui.text_edit_singleline(&mut self.cfg.settings.extra_jvm_args);
                        ui.end_row();

                        ui.label("自动下载 Java");
                        ui.checkbox(&mut self.cfg.settings.auto_download_java, "");
                        ui.end_row();

                        ui.label("数据目录");
                        ui.weak(self.layout.root().display().to_string());
                        ui.end_row();
                    });

                    ui.add_space(10.0);
                    ui.separator();
                    ui.heading("实例储存位置");
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.instances_dir_input)
                                .desired_width(400.0)
                                .hint_text("留空 = 数据目录下的 instances"),
                        );
                        if ui.button("浏览…").clicked() {
                            if let Some(dir) = rfd::FileDialog::new()
                                .set_title("选择实例储存位置")
                                .pick_folder()
                            {
                                self.instances_dir_input = dir.to_string_lossy().to_string();
                            }
                        }
                        if ui.button("应用并迁移").clicked() {
                            pending = Some(Action::ApplyInstancesDir);
                        }
                        if ui.button("打开").clicked() {
                            pending = Some(Action::OpenInstancesDir);
                        }
                    });
                    ui.weak(format!(
                        "当前：{}　·　切换时会把已有实例一起搬过去，失败自动回滚；游戏运行时请先关掉游戏",
                        self.layout.instances_dir().display()
                    ));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("保存设置").clicked() {
                            pending = Some(Action::SaveSettings);
                        }
                        if ui.button("重新拉取版本清单").clicked() {
                            self.manifest_loaded = false;
                            self.load_manifest();
                        }
                    });
                }
            }
        });

        if self.show_login {
            self.login_window(ctx, &mut pending);
        }
        if self.show_new {
            self.new_instance_window(ctx, &mut pending);
        }
        if self.confirm_delete.is_some() {
            self.delete_confirm_window(ctx, &mut pending);
        }
        if let Some(plan) = self.dependency_plan.as_mut() {
            let mut open = true;
            let mut close = false;
            let mut confirm = false;
            egui::Window::new("安装前置模组").collapsible(false).open(&mut open).show(ctx, |ui| {
                ui.label(format!("{} 需要以下前置：", plan.target_title));
                for dep in &mut plan.candidates {
                    let icon = if dep.dependency_type == "required" { "🔴" } else { "🔵" };
                    let state = if dep.status == "installed" { "✅ 已安装" } else { "⚠️ 未安装" };
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut dep.selected, format!("{icon} {} · {} · {}", dep.title, state, dep.recommended_version));
                    });
                }
                if self.dependency_warning { ui.colored_label(egui::Color32::YELLOW, "已取消必需前置，目标 Mod 可能无法启动。再次点击确认继续。"); }
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() { close = true; }
                    if ui.button("确认安装").clicked() { confirm = true; }
                });
            });
            if confirm {
                let missing_required = plan.candidates.iter().any(|d| d.dependency_type == "required" && d.status != "installed" && !d.selected);
                if missing_required && !self.dependency_warning { self.dependency_warning = true; }
                else {
                    let target = plan.target_project_id.clone();
                    let deps = plan.candidates.iter().filter(|d| d.selected && d.status != "installed").map(|d| d.project_id.clone()).collect();
                    pending = Some(Action::InstallWithDependencies { target, dependencies: deps });
                    open = false;
                }
            }
            if close { open = false; }
            if !open { self.dependency_plan = None; self.dependency_warning = false; }
        }
        if let Some(message) = self.install_notice.clone() {
            let mut open = true;
            let mut close = false;
            egui::Window::new("安装完成")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(message);
                    ui.add_space(6.0);
                    if ui.button("确定").clicked() {
                        close = true;
                    }
                });
            if close || !open {
                self.install_notice = None;
            }
        }
        if let Some((code, uri, message)) = self.device.clone() {
            let mut open = true;
            egui::Window::new("微软账号授权")
                .collapsible(false)
                .open(&mut open)
                .show(ctx, |ui| {
                    ui.label(message);
                    ui.horizontal(|ui| {
                        ui.strong(&code);
                        if ui.button("复制").clicked() {
                            ui.output_mut(|o| o.copied_text = code.clone());
                        }
                    });
                    ui.hyperlink_to("打开授权页面", &uri);
                    ui.separator();
                    if ui.button("我已授权，继续").clicked() {
                        ui.output_mut(|o| o.copied_text = code.clone());
                    }
                });
            if !open {
                self.device = None;
            }
        }

        if let Some(a) = pending {
            self.handle(a, ctx);
        }
        if self.cfg_changed {
            self.cfg_changed = false;
            self.persist();
        }
    }
}

impl NblApp {
    fn login_window(&mut self, ctx: &egui::Context, pending: &mut Option<Action>) {
        let mut open = self.show_login;
        egui::Window::new("添加账号")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.heading("微软账号");
                ui.label("推荐：可以玩正版服务器，登录状态会长期保存。");
                if ui.button("使用微软账号登录").clicked() {
                    *pending = Some(Action::LoginMs);
                }
                ui.separator();
                ui.heading("离线账号");
                ui.label("只用于离线/局域网，用户名 3–16 位。");
                ui.text_edit_singleline(&mut self.offline_name);
                if ui.button("添加离线账号").clicked() {
                    *pending = Some(Action::AddOffline);
                }
                ui.separator();
                if !self.cfg.accounts.is_empty() {
                    ui.label("已有账号:");
                    let accounts = self.cfg.accounts.clone();
                    for a in accounts {
                        ui.weak(format!(
                            "{} ({})",
                            a.name,
                            config::account_kind_label(&a.kind)
                        ));
                    }
                }
            });
        self.show_login = open;
    }

    fn delete_confirm_window(&mut self, ctx: &egui::Context, pending: &mut Option<Action>) {
        let Some((id, name, dir)) = self.confirm_delete.clone() else {
            return;
        };
        let mut open = true;
        let mut confirmed = false;
        let mut cancelled = false;
        egui::Window::new("删除实例")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(format!("确定删除实例「{name}」吗？"));
                ui.add_space(4.0);
                ui.label("会把这个实例的整个目录一起删掉，包含存档、mods、配置：");
                ui.monospace(dir.display().to_string());
                ui.add_space(4.0);
                ui.colored_label(
                    egui::Color32::from_rgb(220, 90, 90),
                    "删除后无法恢复。",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add(
                            egui::Button::new("删除")
                                .fill(egui::Color32::from_rgb(170, 40, 40)),
                        )
                        .clicked()
                    {
                        confirmed = true;
                    }
                    if ui.button("取消").clicked() {
                        cancelled = true;
                    }
                });
            });
        if confirmed {
            self.confirm_delete = None;
            *pending = Some(Action::ConfirmDelete(id));
        } else if cancelled || !open {
            self.confirm_delete = None;
        }
    }

    fn new_instance_window(&mut self, ctx: &egui::Context, pending: &mut Option<Action>) {
        let mut open = self.show_new;
        let mut want_loaders = false;
        egui::Window::new("新建实例")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("名称");
                    ui.text_edit_singleline(&mut self.form_name);
                });
                ui.horizontal(|ui| {
                    ui.label("游戏版本");
                    let text = if self.form_mc.is_empty() {
                        "加载中…".to_string()
                    } else {
                        self.form_mc.clone()
                    };
                    egui::ComboBox::from_label("游戏版本选择")
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            let versions = self.versions.clone();
                            for v in versions {
                                ui.selectable_value(&mut self.form_mc, v.clone(), v);
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label("加载器");
                    let before = self.form_loader;
                    egui::ComboBox::from_label("加载器选择")
                        .selected_text(self.form_loader.label())
                        .show_ui(ui, |ui| {
                            for k in LoaderKind::all() {
                                ui.selectable_value(&mut self.form_loader, k, k.label());
                            }
                        });
                    if before != self.form_loader {
                        want_loaders = true;
                    }
                });
                if self.form_loader != LoaderKind::Vanilla {
                    ui.horizontal(|ui| {
                        ui.label("加载器版本");
                        let text = if self.form_loader_ver.is_empty() {
                            "最新".to_string()
                        } else {
                            self.form_loader_ver.clone()
                        };
                        egui::ComboBox::from_label("加载器版本选择")
                            .selected_text(text)
                            .show_ui(ui, |ui| {
                                let list = self.loader_versions.clone();
                                ui.selectable_value(
                                    &mut self.form_loader_ver,
                                    String::new(),
                                    "最新",
                                );
                                for v in list {
                                    ui.selectable_value(&mut self.form_loader_ver, v.clone(), v);
                                }
                            });
                        if ui.button("拉取版本列表").clicked() {
                            want_loaders = true;
                        }
                    });
                }
                ui.separator();
                ui.label("提示：Fabric/Quilt 秒装；Forge/NeoForge 需要跑官方安装器，首次会久一些。");
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("创建并安装").clicked() {
                        *pending = Some(Action::CreateInstance);
                    }
                    if ui.button("取消").clicked() {
                        self.show_new = false;
                    }
                });
            });
        if want_loaders {
            *pending = Some(Action::LoadLoaderVersions);
        }
        self.show_new = open;
    }
}

fn open_in_file_manager(dir: &std::path::Path) {
    #[cfg(windows)]
    let _ = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
}

fn setup_fonts(ctx: &egui::Context) {
    let candidates = [
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/Deng.ttf",
        "C:/Windows/Fonts/NotoSansSC-VF.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    ];
    for path in candidates {
        if !path.ends_with(".ttf") {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
}

pub fn run(layout: Layout) -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1180.0, 780.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("NewBee Launcher"),
        ..Default::default()
    };
    eframe::run_native(
        "NewBee Launcher",
        options,
        Box::new(move |cc| Ok(Box::new(NblApp::new(cc, layout)))),
    )
}