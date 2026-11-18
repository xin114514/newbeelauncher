//! Small helpers: hashing, timestamps, java discovery.

use anyhow::{anyhow, bail, Context, Result};
use sha1::{Digest, Sha1};
use std::path::{Path, PathBuf};
use std::process::Stdio;

pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn sha1_hex(data: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(data);
    hex::encode(h.finalize())
}

pub fn sha1_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha1::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// Offline-mode UUID, identical to what vanilla derives from a player name.
pub fn offline_uuid(name: &str) -> String {
    use md5::{Digest as _, Md5};
    let mut h = Md5::new();
    h.update(format!("OfflinePlayer:{name}").as_bytes());
    let mut b = h.finalize().to_vec();
    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;
    hex::encode(b)
}

/// `550e8400e29b41d4a716446655440000` -> `550e8400-e29b-41d4-a716-446655440000`
pub fn dashed_uuid(id: &str) -> String {
    let s: String = id.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if s.len() != 32 {
        return id.to_string();
    }
    format!(
        "{}-{}-{}-{}-{}",
        &s[0..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

pub fn now_iso() -> String {
    chrono::Local::now().to_rfc3339()
}

pub fn human_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// Mojang library name -> `group:artifact:version[:classifier][@ext]` parts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MavenName {
    pub group: String,
    pub artifact: String,
    pub version: String,
    pub classifier: Option<String>,
    pub ext: String,
}

impl MavenName {
    pub fn parse(name: &str) -> Result<Self> {
        let (body, ext) = match name.split_once('@') {
            Some((b, e)) => (b, e.to_string()),
            None => (name, "jar".to_string()),
        };
        let parts: Vec<&str> = body.split(':').collect();
        if parts.len() < 3 {
            bail!("bad maven coordinate: {name}");
        }
        Ok(Self {
            group: parts[0].to_string(),
            artifact: parts[1].to_string(),
            version: parts[2].to_string(),
            classifier: parts.get(3).map(|s| s.to_string()),
            ext,
        })
    }

    pub fn path(&self) -> String {
        let group_path = self.group.replace('.', "/");
        match &self.classifier {
            Some(c) => format!(
                "{}/{}/{}/{}-{}-{}.{}",
                group_path, self.artifact, self.version, self.artifact, self.version, c, self.ext
            ),
            None => format!(
                "{}/{}/{}/{}-{}.{}",
                group_path, self.artifact, self.version, self.artifact, self.version, self.ext
            ),
        }
    }

    /// `1.20.1-forge-47.2.0` -> some loaders key off this.
    pub fn is_platform(&self) -> bool {
        self.artifact.contains("natives") || self.classifier.is_some()
    }
}

/// Major version of a java binary (`21`, `17`, `1.8` -> `8`).
pub fn java_major(java: &Path) -> Result<u32> {
    let out = std::process::Command::new(java)
        .arg("-XshowSettings:properties")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("running {}", java.display()))?;
    let text = String::from_utf8_lossy(&out.stderr);
    parse_java_major(&text)
        .ok_or_else(|| anyhow!("could not parse java version from {}", java.display()))
}

fn parse_java_major(text: &str) -> Option<u32> {
    // java.specification.version = 21
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("java.specification.version") {
            let v = rest.trim_start_matches([' ', '=']).trim();
            if let Some(major) = v.split('.').next() {
                if let Ok(n) = major.parse::<u32>() {
                    return Some(n);
                }
            }
        }
    }
    // version "1.8.0_402"
    for line in text.lines() {
        if let Some(idx) = line.find("version \"") {
            let rest = &line[idx + 9..];
            let v: String = rest.chars().take_while(|c| *c != '"').collect();
            let mut it = v.split('.');
            let first = it.next()?.parse::<u32>().ok()?;
            if first == 1 {
                return it.next()?.parse::<u32>().ok();
            }
            return Some(first);
        }
    }
    None
}

/// A java binary together with the major version it reports.
#[derive(Debug, Clone)]
pub struct FoundJava {
    pub path: PathBuf,
    pub major: u32,
}

fn java_exe_name() -> &'static str {
    if cfg!(windows) {
        "java.exe"
    } else {
        "java"
    }
}

/// Collect `bin/java` executables under `root`, walking at most `max_depth` levels.
fn collect_java_bins(root: &Path, max_depth: u32, out: &mut Vec<PathBuf>) {
    if max_depth == 0 || !root.is_dir() {
        return;
    }
    let exe = root.join("bin").join(java_exe_name());
    if exe.is_file() {
        out.push(exe);
    }
    let Ok(rd) = std::fs::read_dir(root) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_java_bins(&path, max_depth - 1, out);
        }
    }
}

/// Where Mojang's own launcher keeps the runtimes it downloaded, so we can reuse
/// them instead of fetching another copy.
fn mojang_runtime_roots() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(d) = dirs::data_dir() {
        out.push(d.join(".minecraft").join("runtime"));
        out.push(d.join("minecraft").join("runtime")); // macOS naming
    }
    if let Some(h) = dirs::home_dir() {
        out.push(h.join(".minecraft").join("runtime"));
    }
    out.retain(|p| p.is_dir());
    out
}

/// Best-effort java hunt: explicit path, our own runtime dir, Mojang's launcher
/// runtimes, JAVA_HOME, PATH and the usual install locations.
///
/// Only binaries whose major version is `>= want_major` are accepted — returning
/// a too-old JVM here would make the caller believe Java is ready and skip the
/// "download it" path, which ends in `UnsupportedClassVersionError` at runtime.
pub fn find_java(
    explicit: Option<&Path>,
    java_root: &Path,
    want_major: u32,
) -> Result<FoundJava> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = explicit {
        if p.is_file() {
            candidates.push(p.to_path_buf());
        } else if p.is_dir() {
            candidates.push(p.join("bin").join(java_exe_name()));
        }
    }

    // our own runtimes: <java_root>/<component>/<platform>/bin/java
    collect_java_bins(java_root, 3, &mut candidates);
    // runtimes another launcher already installed
    for root in mojang_runtime_roots() {
        collect_java_bins(&root, 3, &mut candidates);
    }

    if let Ok(home) = std::env::var("JAVA_HOME") {
        if !home.trim().is_empty() {
            candidates.push(PathBuf::from(home).join("bin").join(java_exe_name()));
        }
    }
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let c = dir.join(java_exe_name());
            if c.is_file() {
                candidates.push(c);
            }
        }
    }
    for pat in [
        "C:/Program Files/Java",
        "C:/Program Files/Eclipse Adoptium",
        "C:/Program Files/Microsoft",
        "C:/Program Files/Zulu",
        "C:/Program Files/BellSoft",
        "C:/Program Files/Amazon Corretto",
    ] {
        collect_java_bins(Path::new(pat), 2, &mut candidates);
    }

    let mut best: Option<FoundJava> = None;
    let mut best_score = u32::MAX;
    let mut highest_low: Option<(PathBuf, u32)> = None;
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for c in candidates {
        if !c.is_file() || !seen.insert(c.clone()) {
            continue;
        }
        let Ok(major) = java_major(&c) else { continue };
        if major < want_major {
            if highest_low.as_ref().map(|(_, m)| major > *m).unwrap_or(true) {
                highest_low = Some((c, major));
            }
            continue;
        }
        let score = major - want_major;
        if score < best_score {
            best_score = score;
            best = Some(FoundJava { path: c, major });
        }
    }

    match best {
        Some(found) => Ok(found),
        None => {
            let hint = match highest_low {
                Some((p, m)) => format!("；盘上最高的只有 Java {m}（{}）", p.display()),
                None => String::new(),
            };
            Err(anyhow!(
                "需要 Java {want_major}+，没有找到可用的 Java 运行时{hint}"
            ))
        }
    }
}

/// Class-file major inside a jar, mapped to the Java release that produced it
/// (69 -> 25). Newer version JSONs leave `javaVersion.major_version` at 0, so the
/// jar itself is the reliable source of truth.
pub fn java_major_from_jar(jar: &Path) -> Result<u32> {
    use std::io::Read;
    let file = std::fs::File::open(jar).with_context(|| format!("open {}", jar.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).with_context(|| format!("read {}", jar.display()))?;
    for i in 0..zip.len() {
        let Ok(mut entry) = zip.by_index(i) else {
            continue;
        };
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if !name.ends_with(".class") || name.starts_with("META-INF/") {
            continue;
        }
        let mut head = [0u8; 8];
        if entry.read_exact(&mut head).is_err() {
            continue;
        }
        if head[0..4] != [0xCA, 0xFE, 0xBA, 0xBE] {
            continue;
        }
        let major = u16::from_be_bytes([head[6], head[7]]) as u32;
        if major < 45 {
            continue;
        }
        return Ok(major - 44);
    }
    bail!("{} 里没有可读的 class 文件", jar.display())
}

/// Mojang runtime component names map to the Java major they ship.
pub fn component_java_major(component: &str) -> Option<u32> {
    Some(match component {
        "jre-legacy" => 8,
        "java-runtime-alpha" => 16,
        "java-runtime-beta" | "java-runtime-gamma" | "java-runtime-gamma-snapshot" => 17,
        "java-runtime-delta" => 21,
        "java-runtime-epsilon" => 25,
        _ => return None,
    })
}

/// What Java a version actually needs: declared metadata, then the class files
/// in the client jar, then the runtime component name.
pub fn required_java_major(
    declared: Option<u32>,
    component: Option<&str>,
    client_jar: Option<&Path>,
) -> u32 {
    if let Some(m) = declared.filter(|m| *m > 0) {
        return m;
    }
    if let Some(jar) = client_jar {
        if let Ok(m) = java_major_from_jar(jar) {
            return m;
        }
    }
    if let Some(c) = component {
        if let Some(m) = component_java_major(c) {
            return m;
        }
    }
    8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maven_paths() {
        let m = MavenName::parse("org.ow2.asm:asm:9.6").unwrap();
        assert_eq!(m.path(), "org/ow2/asm/asm/9.6/asm-9.6.jar");
        let n = MavenName::parse("net.minecraftforge:forge:1.20.1-47.2.0:universal").unwrap();
        assert_eq!(
            n.path(),
            "net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-universal.jar"
        );
        let z = MavenName::parse("a.b:c:1@zip").unwrap();
        assert_eq!(z.ext, "zip");
    }

    #[test]
    fn uuid_formatting() {
        assert_eq!(
            dashed_uuid("f3d28cb072253cb1baeb2dadd2be89ae"),
            "f3d28cb0-7225-3cb1-baeb-2dadd2be89ae"
        );
        assert_eq!(offline_uuid("Notch").len(), 32);
    }

    #[test]
    fn parses_java_version_output() {
        let text = "Property settings:\n    java.specification.version = 21\n";
        assert_eq!(parse_java_major(text), Some(21));
        let old = "java version \"1.8.0_402\"\n";
        assert_eq!(parse_java_major(old), Some(8));
    }
}