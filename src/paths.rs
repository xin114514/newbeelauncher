//! On-disk layout.
//!
//! ```text
//! <root>/
//!   config.json
//!   shared/            # data every instance can share
//!     versions/<id>/<id>.json, <id>.jar
//!     libraries/…
//!     assets/{indexes,objects,log_configs}
//!   java/              # jre's fetched from Mojang
//!   instances/<id>/
//!     game/            # this instance's gameDir (mods, saves, options.txt…)
//!     natives/         # extracted jni libs
//!   cache/             # installers, metadata cache
//! ```

use anyhow::Result;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
    /// Where instances live; `None` means `<root>/instances`.
    instances_root: Option<PathBuf>,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            instances_root: None,
        }
    }

    /// Point the instance store somewhere else (absolute path).
    pub fn with_instances_root(mut self, dir: Option<PathBuf>) -> Self {
        self.instances_root = dir.filter(|p| !p.as_os_str().is_empty());
        self
    }

    pub fn instances_override(&self) -> Option<&Path> {
        self.instances_root.as_deref()
    }

    /// `%LOCALAPPDATA%\NewBeeLauncher` (or `$NEWBEE_HOME`).
    pub fn default_root() -> PathBuf {
        if let Ok(p) = std::env::var("NEWBEE_HOME") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        let base = dirs::data_local_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("NewBeeLauncher")
    }

    pub fn default_layout() -> Self {
        Self::new(Self::default_root())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn shared(&self) -> PathBuf {
        self.root.join("shared")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.shared().join("versions")
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.shared().join("libraries")
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.shared().join("assets")
    }

    pub fn assets_objects_dir(&self) -> PathBuf {
        self.assets_dir().join("objects")
    }

    pub fn assets_indexes_dir(&self) -> PathBuf {
        self.assets_dir().join("indexes")
    }

    pub fn assets_virtual_dir(&self, id: &str) -> PathBuf {
        self.assets_dir().join("virtual").join(id)
    }

    pub fn assets_log_configs_dir(&self) -> PathBuf {
        self.assets_dir().join("log_configs")
    }

    pub fn java_dir(&self) -> PathBuf {
        self.root.join("java")
    }

    pub fn instances_dir(&self) -> PathBuf {
        match &self.instances_root {
            Some(p) => p.clone(),
            None => self.root.join("instances"),
        }
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.root.join("cache")
    }

    pub fn meta_dir(&self) -> PathBuf {
        self.root.join("meta")
    }

    pub fn version_dir(&self, id: &str) -> PathBuf {
        self.versions_dir().join(id)
    }

    pub fn version_json(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.json"))
    }

    pub fn version_jar(&self, id: &str) -> PathBuf {
        self.version_dir(id).join(format!("{id}.jar"))
    }

    pub fn instance_dir(&self, id: &str) -> PathBuf {
        self.instances_dir().join(id)
    }

    /// The isolated `--gameDir` of an instance.
    pub fn instance_game_dir(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("game")
    }

    pub fn instance_natives_dir(&self, id: &str) -> PathBuf {
        self.instance_dir(id).join("natives")
    }

    pub fn instance_mods_dir(&self, id: &str) -> PathBuf {
        self.instance_game_dir(id).join("mods")
    }

    pub fn instance_resourcepacks_dir(&self, id: &str) -> PathBuf {
        self.instance_game_dir(id).join("resourcepacks")
    }

    pub fn instance_shaderpacks_dir(&self, id: &str) -> PathBuf {
        self.instance_game_dir(id).join("shaderpacks")
    }

    pub fn instance_config_dir(&self, id: &str) -> PathBuf {
        self.instance_game_dir(id).join("config")
    }

    pub fn instance_logs_dir(&self, id: &str) -> PathBuf {
        self.instance_game_dir(id).join("logs")
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn ensure(&self) -> Result<()> {
        for d in [
            self.shared(),
            self.versions_dir(),
            self.libraries_dir(),
            self.assets_dir(),
            self.assets_objects_dir(),
            self.assets_indexes_dir(),
            self.assets_log_configs_dir(),
            self.java_dir(),
            self.instances_dir(),
            self.cache_dir(),
            self.meta_dir(),
        ] {
            std::fs::create_dir_all(&d)?;
        }
        Ok(())
    }

    pub fn ensure_instance(&self, id: &str) -> Result<()> {
        for d in [
            self.instance_game_dir(id),
            self.instance_natives_dir(id),
            self.instance_mods_dir(id),
            self.instance_resourcepacks_dir(id),
            self.instance_shaderpacks_dir(id),
            self.instance_logs_dir(id),
            self.instance_config_dir(id),
        ] {
            std::fs::create_dir_all(&d)?;
        }
        Ok(())
    }
}