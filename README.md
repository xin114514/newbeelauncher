# NewBee Launcher

一个用 Rust 从零写的 Minecraft 启动器：**登录状态长期保留**、**每个实例版本隔离**、**直接拉取 Modrinth 的模组/资源包/光影列表**、**一键安装原版 + 任意模组加载器 + 内容**。

带 GUI（egui，单文件原生窗口，无 Electron/WebView）和 CLI（`nbl-cli`）两套入口。

```
┌──────────────────────────────────────────────────────────────┐
│ NewBee Launcher   [账号: Steve(微软)]  [+添加]  ▶ 启动游戏    │
├─────────────┬────────────────────────────────────────────────┤
│ 实例         │ 概览 │ 内容 │ 日志 │ 设置                     │
│ 1.21.1-Fabric│                                                │
│ 1.20.1-Forge │ 1.21.1 · Fabric 0.16.9 · 游戏目录 …            │
│ 1.12.2-原版  │ [内存 4096MB] [打开目录] [刷新内容]            │
└─────────────┴────────────────────────────────────────────────┘
```

## 特性

| 需求 | 实现 |
| --- | --- |
| 长期保留登录状态 | 微软设备码 OAuth2 → Xbox Live → XSTS → Minecraft Services；`refresh_token` 落盘并用 **Windows DPAPI 加密**，每次启动自动续期，过期自动刷新，无需重登 |
| 不同版本相互隔离 | 每个实例独立 `game/` 目录（mods、saves、config、options.txt、resourcepacks 全隔离），只共享 `libraries/`、`assets/`、`versions/` 这些只读文件，省磁盘又不串档 |
| 实例可以放任意盘 | 「设置 → 实例储存位置」可指定绝对路径（默认 `<数据目录>/instances`），点「应用并迁移」会把已有实例整体搬过去，同名冲突或中途失败自动回滚、不改配置。CLI：`nbl-cli instances-dir D:\MC\instances` |
| 删除实例 = 真删文件 | 「删除实例…」先弹确认框列出待删目录，确认后把 `instances/<id>` 整个删掉（存档、mods、配置一起走），共享的 `shared/` 保留。CLI：`nbl-cli delete 生存服` |
| 拉取各加载器的 mod 列表 | Modrinth v2 搜索按 `project_type` / `versions` / `categories(loader)` 三个 facet 组合筛选，Forge/Fabric/Quilt/NeoForge 各有独立列表；同一套 UI 也用于资源包和光影 |
| 一键下载 | 原版（client jar + 全部 libraries + 全部 assets 对象 + log4j 配置 + Mojang 官方 Java 运行时）、Fabric/Quilt（profile → 自动补齐继承链）、Forge/NeoForge（官方 headless 安装器）、Modrinth 内容（含 required 依赖递归安装） |

其他：sha1 校验 + 断点续传式跳过、多线程下载（默认 16 并发）、下载失败重试、进度条、完整日志、离线账号、内存/JVM 参数可按实例配置、`cargo build` 后单 exe 运行。

## 构建

```bash
cargo build --release
# 产物
#   target/release/nbl.exe      GUI
#   target/release/nbl-cli.exe  CLI
```

只需要 Rust 工具链本身（MSVC 或 `x86_64-pc-windows-gnu` 都行）；依赖全是纯 Rust，TLS 走系统 schannel，不用装 OpenSSL / ring 的 C 编译环境。首次构建建议配个 crates.io 镜像（国内直连 `static.crates.io` 常常卡住）：

```toml
# ~/.cargo/config.toml
[source.crates-io]
replace-with = 'rsproxy-sparse'
[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
```

## 使用

### GUI

```bash
nbl.exe
```

1. 顶部「添加账号」：微软登录会弹设备码窗口，浏览器授权一次后长期有效；也可加离线账号。
2. 左侧「+ 新建」：选游戏版本 + 加载器（Vanilla/Fabric/Quilt/Forge/NeoForge）+ 加载器版本，点「创建并安装」。
3. 「启动游戏」。首次启动会自动准备 Java 运行时。
4. 「内容」页：选类型（模组/资源包/光影）→ 搜索 Modrinth → 点「安装」，自动放进当前实例对应的目录。

### CLI

```bash
nbl-cli versions                                  # 官方正式版本列表
nbl-cli install 1.21.1 --loader fabric --name 生存服
nbl-cli install 1.20.1 --loader forge --loader-version 47.2.0
nbl-cli login                                     # 设备码登录，长期保存
nbl-cli add-offline Steve
nbl-cli list
nbl-cli launch 生存服                              # 用当前账号启动
nbl-cli launch 生存服 --offline Steve
nbl-cli search sodium --type mod --mc 1.21.1 --loader fabric
nbl-cli add 生存服 AANobbMI                       # 按 Modrinth 项目 id 安装
nbl-cli java 生存服                               # 查这个实例要哪个 Java、实际选谁
nbl-cli instances-dir                             # 看实例存在哪
nbl-cli instances-dir D:\Minecraft\instances      # 换位置并把已有实例搬过去
nbl-cli delete 生存服                             # 删除实例并删文件（会问一次 y/N）
nbl-cli delete 生存服 --yes                       # 不问直接删
nbl-cli installed
nbl-cli info
```

### 数据目录

默认 `%LOCALAPPDATA%\NewBeeLauncher`，用环境变量 `NEWBEE_HOME` 可改：

```
<root>/
  config.json                    账号、实例、设置（refresh token 已加密）
  shared/
    versions/<id>/{<id>.json,<id>.jar}
    libraries/…
    assets/{indexes,objects,log_configs}
  java/<component>/<platform>/     Mojang 官方 Java 运行时
  instances/<实例 id>/             ← 可用 settings.instances_dir 指到别的盘
    game/                         ← 这个实例的 --gameDir（隔离）
      mods/ resourcepacks/ shaderpacks/ saves/ config/ logs/
    natives/                      ← jni 本地库解压目录
  cache/                          安装器缓存
  meta/                           版本清单缓存
```

## 代码结构

```
src/
  main.rs              GUI 入口
  bin/cli.rs           CLI 入口
  lib.rs               Ctx（http/布局/事件）
  paths.rs             目录布局 = 隔离策略
  config.rs            账号/实例/设置的持久化
  auth.rs              微软设备码登录链 + DPAPI 加密的 token 保管
  net.rs               并发下载器（sha1 校验 / 重试 / 进度事件）
  mojang.rs            版本清单、version json、inheritsFrom 合并、规则求值、Java 运行时清单
  install.rs           原版安装：client、libraries、natives、assets、log 配置、JRE
  loaders/             fabric.rs（Fabric/Quilt）、forge.rs（Forge/NeoForge headless 安装）
  launch.rs            参数模板替换 → 命令行 → 子进程
  modrinth.rs          搜索/版本/下载/依赖
  gui.rs               egui 界面
```

## 说明与已知限制

- **微软 Client ID**：默认用社区公共客户端 `c36a9fb6-4f2a-41ff-90bd-ae7cc92031eb`（PrismLauncher 那份，支持设备码）。微软已下架旧启动器的 `00000000402b5328`，现在用它会直接返回 `AADSTS700016`；老配置里的这个值会在启动时自动迁移，登录时还会自动回退到可用 id。要完全自控，就在「设置 → 微软 Client ID」填你自己在 Azure 注册的**公共客户端** ID。
- **Java 版本按 class 文件判定，绝不降级凑数**：新版 version JSON 的 `javaVersion.major_version` 经常是 `0`（26.3 就是 0），所以启动器直接读客户端 jar 里 class 文件的 major（`69 → Java 25`），再回退到 runtime 组件名。找不到满足版本的 JDK 时返回错误并走 Mojang 官方 JRE 下载（`java-runtime-epsilon` = Java 25），而不是拿 Java 21 硬上 —— 那正是 `UnsupportedClassVersionError: class file version 69.0` 的来源。别的启动器已经装好的 `%APPDATA%\.minecraft\runtime` 会被直接复用，不会重复下载。
- **实例搬家是"全有或全无"**：切换实例目录时先全部移动，任何一个目录失败或目标已有同名实例就整体回滚、不改 `config.json`，不会出现"配置指向新目录但旧实例留在原地"的鬼状态。同一盘内是 `rename`（瞬间完成），跨盘才走复制+删除。
- **没有黑色 cmd 窗口**：`nbl.exe` 编译成 Windows GUI 子系统（`windows_subsystem = "windows"`），启动器本身不带控制台；拉起游戏/安装器时用 `CREATE_NO_WINDOW`，只出现游戏窗口。
- **Forge / NeoForge**：走官方安装器的 `--installClient`，需要 Java 17+；首次安装会跑 patcher 并下载上百个库，日志里能看到安装器输出。
- **老版本（1.6 之前）**：走 `--assetsDir` 虚拟目录模式（`assets/virtual/<index>` 复制出来），能启动，但会多占磁盘。
- **Modrinth 依赖**：只自动装 `required` 依赖，`optional` 由用户自选；整合包（modpack）暂不解析 `.mrpack`。
- **删除实例是破坏性操作**：GUI 的「删除实例…」会弹出确认框，把目标目录完整列出来再让你点红色「删除」；CLI 默认要求输入 `y` 确认。确认后 `instances/<id>` 整棵目录（存档、mods、配置、natives）被删掉，只保留 `shared/` 里共享的 versions/libraries/assets。游戏正在运行时删除会失败并提示先关游戏，实例记录不会丢。

### 两个必须踩对的地方（实现笔记）

1. **Forge 安装器的工作目录**：`--installClient <dir>` 会把 `versions/`、`libraries/` 写到传入目录下，且要求该目录里有 `launcher_profiles.json`（否则直接报 "no Minecraft launcher profile"）。所以必须传 **共享目录** `<root>/shared`，并预先写入最小 profile 文件。
2. **Forge/NeoForge 1.17+ 的 classpath 不能带 vanilla jar**：它们的 `BootstrapLauncher` 通过 `-DlibraryDirectory` 自己构造 `minecraft` 模块（来自 `forge-*-client.jar`）。如果启动器再把 `versions/1.20.1/1.20.1.jar` 塞进 `-cp`，两个模块会同时宣称拥有 `com.mojang.blaze3d.*`，启动直接 `ResolutionException`。`prepare()` 里按 `mainClass` 是否含 `bootstraplauncher` 决定要不要追加客户端 jar。

另：`arguments.jvm` 里的 `${library_directory}`、`${classpath_separator}` 必须替换成真实值（前者是 `libraries/` 根，后者 Windows 下是 `;`），否则 Forge 的 `-p` 模块路径会拼成一串无效字符串。

## 实测验证

在本机（Windows 11，Intel Iris Xe，JDK 21）实际跑通：

| 链路 | 结果 |
| --- | --- |
| 原版 1.21.1 | 70 个库 + 3913 个资源对象 + client jar 全量下载，启动进渲染（atlas 构建完成） |
| Fabric 0.20.1 + Sodium | `Loading Minecraft 1.21.1 with Fabric Loader 0.20.1`，Sodium 正常加载，OpenGL 正常 |
| Quilt 0.31.0-beta.6 | profile 拉取、继承链补齐、依赖下载全部通过 |
| Forge 47.4.29（1.20.1） | 官方安装器 headless 完成 patcher，ModLauncher 启动成功进入 `Render thread` |
| Modrinth | 搜索 facets（加载器 × 版本 × 类型）返回正确结果，`sodium` 一键装进实例 `mods/` |
| 26.3 + Fabric 0.20.1 | 识别出需要 Java 25（class 69），自动选中本机 `~/.minecraft/runtime/java-runtime-epsilon`，`Loading Minecraft 26.3` 后进 `Backend library: LWJGL 3.4.3` |
| GUI | 窗口启动、账号/实例/内容/日志/设置五个页签渲染正常 |

自查命令（不启动游戏，秒出结果）：

```powershell
nbl-cli java 实例名
# 实例 新实例  (版本 id fabric-loader-0.20.1-26.3)
#   JSON 声明 javaVersion.major_version = Some(0)
#   运行时组件 = Some("java-runtime-epsilon")
#   jar 内 class 文件对应 Java 25
# => 该版本需要 Java 25+
# => 会使用 Java 25 : ...\.minecraft\runtime\java-runtime-epsilon\bin\java.exe
```

## 参考协议

- Minecraft launcher 元数据：`piston-meta.mojang.com/mc/game/version_manifest_v2.json`
- 微软 OAuth2 device code：`login.microsoftonline.com/consumers/oauth2/v2.0/devicecode`
- Fabric meta：`meta.fabricmc.net/v2`，Quilt：`meta.quiltmc.org/v3`
- Forge maven：`maven.minecraftforge.net`，NeoForge：`maven.neoforged.net/releases`
- 内容库：`api.modrinth.com/v2`