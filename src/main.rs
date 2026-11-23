//! GUI entry point.
//!
//! Without this attribute a Rust binary is a console program on Windows and
//! double-clicking it pops a cmd window behind the launcher.
#![cfg_attr(windows, windows_subsystem = "windows")]

use newbeelauncher::paths::Layout;

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let layout = Layout::default_layout();
    if let Err(e) = layout.ensure() {
        eprintln!("无法创建数据目录: {e}");
    }
    if let Err(e) = newbeelauncher::gui::run(layout) {
        eprintln!("启动界面失败: {e}");
        std::process::exit(1);
    }
    Ok(())
}