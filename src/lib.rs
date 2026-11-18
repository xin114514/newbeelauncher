//! NewBee Launcher core.
//!
//! - persistent Microsoft / offline accounts (refresh-token vault)
//! - per-instance isolation (each instance owns its own game directory)
//! - one-click install of vanilla, mod loaders, mods, resource packs

pub mod auth;
pub mod config;
pub mod gui;
pub mod install;
pub mod launch;
pub mod loaders;
pub mod modrinth;
pub mod mojang;
pub mod net;
pub mod paths;
pub mod progress;
pub mod util;

pub use config::{Instance, LauncherConfig, LoaderKind};
pub use paths::Layout;
pub use progress::{Event, ProgressSender};

/// Shared runtime handle: an http client, the on-disk layout and an event sink.
#[derive(Clone)]
pub struct Ctx {
    pub client: reqwest::Client,
    pub layout: Layout,
    pub tx: ProgressSender,
}

impl Ctx {
    pub fn new(layout: Layout, tx: ProgressSender) -> Self {
        Self {
            client: net::client(),
            layout,
            tx,
        }
    }

    /// Same context with another version/instance wired in (kept for clarity).
    pub fn with_layout(&self, layout: Layout) -> Self {
        Self {
            client: self.client.clone(),
            layout,
            tx: self.tx.clone(),
        }
    }

    pub fn log(&self, msg: impl Into<String>) {
        self.tx.log(msg.into());
    }
}