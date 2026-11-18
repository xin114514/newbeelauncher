//! Mod loader support: Fabric, Quilt, Forge and NeoForge.

pub mod fabric;
pub mod forge;

use crate::config::LoaderKind;
use crate::Ctx;
use anyhow::Result;
use std::path::{Path, PathBuf};

/// A loader version the user can pick in the UI.
#[derive(Clone, Debug)]
pub struct LoaderChoice {
    pub version: String,
    pub stable: bool,
}

/// Install (or reuse) a loader for `mc` and return the resulting version id.
pub async fn install(
    ctx: &Ctx,
    mc: &str,
    kind: LoaderKind,
    loader_version: Option<&str>,
    concurrency: usize,
    java: Option<PathBuf>,
) -> Result<String> {
    match kind {
        LoaderKind::Vanilla => {
            crate::install::prepare(ctx, mc, concurrency).await?;
            Ok(mc.to_string())
        }
        LoaderKind::Fabric => {
            fabric::install(ctx, mc, loader_version, concurrency, false).await
        }
        LoaderKind::Quilt => {
            fabric::install(ctx, mc, loader_version, concurrency, true).await
        }
        LoaderKind::Forge => {
            forge::install(ctx, mc, loader_version, concurrency, java.as_deref()).await
        }
        LoaderKind::NeoForge => {
            forge::install_neoforge(ctx, mc, loader_version, concurrency, java.as_deref()).await
        }
    }
}

pub async fn list_versions(
    ctx: &Ctx,
    mc: &str,
    kind: LoaderKind,
) -> Result<Vec<LoaderChoice>> {
    match kind {
        LoaderKind::Vanilla => Ok(vec![]),
        LoaderKind::Fabric => fabric::list_loaders(ctx, mc, false).await,
        LoaderKind::Quilt => fabric::list_loaders(ctx, mc, true).await,
        LoaderKind::Forge => forge::list_forge(ctx, mc).await,
        LoaderKind::NeoForge => forge::list_neoforge(ctx, mc).await,
    }
}

/// Pick a sane default java for the installer (17+ works for every modern loader).
pub async fn installer_java(ctx: &Ctx, concurrency: usize) -> Result<PathBuf> {
    if let Ok(found) = crate::util::find_java(None, &ctx.layout.java_dir(), 17) {
        return Ok(found.path);
    }
    crate::install::ensure_java(ctx, Some("java-runtime-gamma"), 17, concurrency).await
}

#[allow(dead_code)]
fn _unused(_: &Path) {}