//! The environment of the configured Python interpreter: its version, its
//! `EXTENSION_SUFFIXES` and the `sys.path` directories to index.
//!
//! The interpreter is run once per server start, with a single probe script.
//! When it runs in a Docker (or Podman) container (`python_command = "docker exec -i
//! <container> python3"`), the directories of its `sys.path` live in the container:
//! - the ones covered by `path_mappings` (volumes) are read at their host path;
//! - the others are copied once per image (`docker cp`) to the cache directory, without
//!   bytecode and with compiled extensions reduced to empty placeholders, and indexed there.
//!
//! The result is cached on disk, keyed by the command and the mappings, and tagged with
//! the image ID of the container. While the image does not change, a start only runs
//! `docker inspect`, never `docker exec`; when the container is unavailable, the cached
//! environment is used as is.

use std::collections::HashSet;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use rustc_hash::FxHasher;
use serde::{Deserialize, Serialize};

use crate::S;

/// Prints, as one JSON line, everything the server needs from the interpreter. The
/// entries of each `sys.path` directory let a container copy leave packages out.
pub const PROBE_SCRIPT: &str = "import sys, os, json, importlib.machinery; \
p = [p for p in sys.path if p and os.path.isdir(p)]; \
print(json.dumps({'version_info': list(sys.version_info)[:3], \
'ext_suffixes': list(importlib.machinery.EXTENSION_SUFFIXES), \
'sys_path': p, 'entries': {d: sorted(os.listdir(d)) for d in p}}))";

/// Bump when the cache layout or the copy filter changes: older caches are then ignored.
const CACHE_FORMAT: u32 = 2;

/// Top-level packages left out of the copies. The server reads Odoo from `odoo_path`,
/// which has to be on this machine; the `odoo` package of an official image's
/// `dist-packages` is most of its size (1.3 of 1.4 GB for Odoo 18).
const NOT_COPIED_PACKAGES: &[&str] = &["odoo"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PythonEnv {
    pub version: Vec<u64>,
    pub ext_suffixes: Vec<String>,
    /// `sys.path` directories, as paths of this machine, in the interpreter's order.
    pub sys_path: Vec<String>,
}

#[derive(Debug)]
pub struct ResolvedPythonEnv {
    pub env: PythonEnv,
    /// The environment comes from the disk cache: the interpreter was not run.
    pub from_cache: bool,
    /// What the user should know: skipped `sys.path` entries, copies, fallbacks.
    pub notes: Vec<String>,
}

/// `container_path=host_path`, as given by `path_mappings`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PathMapping {
    pub container: String,
    pub host: String,
}

impl PathMapping {
    pub fn parse(entry: &str) -> Option<Self> {
        let (container, host) = entry.split_once('=')?;
        let container = container.trim().trim_end_matches('/');
        let host = host.trim();
        if container.is_empty() || host.is_empty() {
            return None;
        }
        Some(Self { container: container.to_string(), host: host.to_string() })
    }
}

/// Splits a command line into words: whitespace separates them, single and double
/// quotes group them, a backslash escapes the next character outside single quotes.
pub fn split_command(command: &str) -> Vec<String> {
    let mut words = vec![];
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') | (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
                in_word = true;
            }
            (Some(_), c) => current.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            (None, c) => {
                current.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

/// Maps a container path through the longest matching mapping.
pub fn map_path(path: &str, mappings: &[PathMapping]) -> Option<String> {
    mappings
        .iter()
        .filter_map(|m| {
            let rest = path.strip_prefix(m.container.as_str())?;
            (rest.is_empty() || rest.starts_with('/')).then_some((m, rest))
        })
        .max_by_key(|(m, _)| m.container.len())
        .map(|(m, rest)| {
            if rest.is_empty() {
                m.host.clone()
            } else {
                Path::new(&m.host).join(rest.trim_start_matches('/')).to_string_lossy().into_owned()
            }
        })
}

/// A `docker exec` / `podman exec` command: the CLI and the container it targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockerExec {
    pub cli: String,
    pub container: String,
}

impl DockerExec {
    pub fn parse(command: &[String]) -> Option<Self> {
        let cli = command.first()?;
        let name = Path::new(cli).file_stem()?.to_str()?;
        if !matches!(name, "docker" | "podman") || command.get(1).map(String::as_str) != Some("exec") {
            return None;
        }
        let mut i = 2;
        while let Some(arg) = command.get(i) {
            if let Some(long) = arg.strip_prefix("--") {
                let takes_value = matches!(long, "env" | "env-file" | "user" | "workdir" | "detach-keys");
                i += if takes_value { 2 } else { 1 };
            } else if arg.len() > 1 && arg.starts_with('-') {
                i += if matches!(arg.as_str(), "-e" | "-u" | "-w") { 2 } else { 1 };
            } else {
                // The container, followed by the interpreter.
                return (command.len() > i + 1).then(|| Self { cli: cli.clone(), container: arg.clone() });
            }
        }
        None
    }

    /// ID of the image the container runs. Works on a stopped container; never enters it.
    pub fn image_id(&self) -> Result<String, String> {
        let output = Command::new(&self.cli)
            .args(["inspect", "--type", "container", "--format", "{{.Image}}", &self.container])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run {}: {e}", self.cli))?;
        let id = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || id.is_empty() {
            return Err(format!(
                "container '{}' not found: {}",
                self.container,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(id)
    }
}

#[derive(Debug, Deserialize)]
struct Probe {
    version_info: Vec<u64>,
    ext_suffixes: Vec<String>,
    sys_path: Vec<String>,
    #[serde(default)]
    entries: std::collections::HashMap<String, Vec<String>>,
}

fn run_probe(command: &[String]) -> Result<Probe, String> {
    let (program, args) = command.split_first().ok_or("empty Python command")?;
    let output = Command::new(program)
        .args(args)
        .args(["-c", PROBE_SCRIPT])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run '{}': {e}", command.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "'{}' failed: {}",
            command.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Wrappers may print before the interpreter: the probe's JSON is the last line.
    let line = stdout.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("");
    serde_json::from_str(line.trim()).map_err(|e| format!("unexpected output of '{}': {e}", command.join(" ")))
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedEnv {
    format: u32,
    command: Vec<String>,
    mappings: Vec<PathMapping>,
    image_id: String,
    env: PythonEnv,
    notes: Vec<String>,
}

fn cache_key(command: &[String], mappings: &[PathMapping]) -> String {
    let mut hasher = FxHasher::default();
    command.hash(&mut hasher);
    mappings.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

/// The image directory name: the digest without its `sha256:` prefix.
fn image_dir_name(image_id: &str) -> String {
    image_id.rsplit(':').next().unwrap_or(image_id).chars().filter(char::is_ascii_alphanumeric).collect()
}

fn read_cache(file: &Path) -> Option<CachedEnv> {
    let cached: CachedEnv = serde_json::from_slice(&fs::read(file).ok()?).ok()?;
    (cached.format == CACHE_FORMAT).then_some(cached)
}

/// Resolves the environment of `command` (the interpreter and its arguments).
/// `cache_root` holds the cached environments and the copies of container directories;
/// without it a container's unmapped `sys.path` entries are skipped.
pub fn resolve(command: &[String], mappings: &[PathMapping], cache_root: Option<&Path>) -> Result<ResolvedPythonEnv, String> {
    match DockerExec::parse(command) {
        Some(docker) => resolve_docker(&docker, command, mappings, cache_root),
        None => resolve_local(command, mappings),
    }
}

fn resolve_local(command: &[String], mappings: &[PathMapping]) -> Result<ResolvedPythonEnv, String> {
    let probe = run_probe(command)?;
    let mut notes = vec![];
    let mut sys_path = vec![];
    for path in probe.sys_path {
        let host = map_path(&path, mappings).unwrap_or_else(|| path.clone());
        if Path::new(&host).is_dir() {
            sys_path.push(host);
        } else {
            notes.push(format!("sys.path entry '{path}' skipped: '{host}' is not a directory here"));
        }
    }
    Ok(ResolvedPythonEnv {
        env: PythonEnv { version: probe.version_info, ext_suffixes: probe.ext_suffixes, sys_path },
        from_cache: false,
        notes,
    })
}

fn resolve_docker(docker: &DockerExec, command: &[String], mappings: &[PathMapping], cache_root: Option<&Path>) -> Result<ResolvedPythonEnv, String> {
    let cache_file = cache_root.map(|root| root.join("envs").join(format!("{}.json", cache_key(command, mappings))));
    let cached = cache_file.as_deref().and_then(read_cache);
    let image_id = docker.image_id();
    match (&image_id, cached) {
        (Ok(id), Some(cached)) if cached.image_id == *id && cached.env.sys_path.iter().all(|p| Path::new(p).is_dir()) => {
            return Ok(ResolvedPythonEnv { env: cached.env, from_cache: true, notes: cached.notes });
        }
        (Err(err), Some(cached)) => {
            let mut notes = cached.notes;
            notes.push(format!("{err}; using the environment cached for image {}", cached.image_id));
            return Ok(ResolvedPythonEnv { env: cached.env, from_cache: true, notes });
        }
        (Err(err), None) => return Err(err.clone()),
        _ => {}
    }
    let image_id = image_id.unwrap();
    let probe = run_probe(command)?;
    // `notes` are cached with the environment; `copy_notes` only concern this run.
    let mut notes = vec![];
    let mut copy_notes = vec![];
    let image_dir = cache_root.map(|root| root.join("images").join(image_dir_name(&image_id)));
    // Parents first: a nested entry (`.../python3.12/lib-dynload`) is then already copied.
    let mut to_copy: Vec<&String> = probe.sys_path.iter().filter(|p| map_path(p, mappings).is_none()).collect();
    to_copy.sort_by_key(|p| p.len());
    let mut copied: HashSet<&str> = HashSet::new();
    if let Some(image_dir) = &image_dir {
        for path in to_copy {
            if Path::new(path.as_str()).parent().is_none() {
                notes.push(format!("sys.path entry '{path}' skipped: refusing to copy a filesystem root"));
                continue;
            }
            let dest = copy_destination(image_dir, path);
            if dest.is_dir() {
                copied.insert(path.as_str());
                continue;
            }
            match copy_from_container(docker, path, &dest, &probe.ext_suffixes, probe.entries.get(path.as_str())) {
                Ok(stats) => {
                    copy_notes.push(format!(
                        "copied '{path}' from container '{}' to '{}' ({} files, {} extension placeholders{})",
                        docker.container, dest.display(), stats.files, stats.placeholders,
                        if stats.skipped_packages.is_empty() { String::new() } else { format!(", without {}", stats.skipped_packages.join(", ")) }
                    ));
                    copied.insert(path.as_str());
                }
                Err(err) => notes.push(format!("sys.path entry '{path}' skipped: {err}")),
            }
        }
    }
    let mut sys_path = vec![];
    for path in &probe.sys_path {
        if let Some(host) = map_path(path, mappings) {
            if Path::new(&host).is_dir() {
                sys_path.push(host);
            } else {
                notes.push(format!("sys.path entry '{path}' skipped: mapped path '{host}' is not a directory"));
            }
        } else if let Some(image_dir) = &image_dir {
            if copied.contains(path.as_str()) {
                sys_path.push(copy_destination(image_dir, path).to_string_lossy().into_owned());
            }
        } else {
            notes.push(format!("sys.path entry '{path}' skipped: no cache directory to copy it to"));
        }
    }
    let env = PythonEnv { version: probe.version_info, ext_suffixes: probe.ext_suffixes, sys_path };
    if let (Some(root), Some(file)) = (cache_root, &cache_file) {
        let cached = CachedEnv {
            format: CACHE_FORMAT,
            command: command.to_vec(),
            mappings: mappings.to_vec(),
            image_id: image_id.clone(),
            env: env.clone(),
            notes: notes.clone(),
        };
        let written = fs::create_dir_all(root.join("envs"))
            .and_then(|_| write_atomic(file, &serde_json::to_vec_pretty(&cached).unwrap_or_default()));
        if let Err(err) = written {
            notes.push(format!("cannot write the cache '{}': {err}", file.display()));
        }
        remove_unused_images(root);
    }
    notes.extend(copy_notes);
    Ok(ResolvedPythonEnv { env, from_cache: false, notes })
}

fn copy_destination(image_dir: &Path, container_path: &str) -> PathBuf {
    let mut dest = image_dir.join("fs");
    for component in Path::new(container_path).components() {
        if let Component::Normal(part) = component {
            dest.push(part);
        }
    }
    dest
}

fn write_atomic(file: &Path, content: &[u8]) -> std::io::Result<()> {
    let tmp = file.with_extension(format!("tmp{}", std::process::id()));
    fs::write(&tmp, content)?;
    fs::rename(&tmp, file)
}

/// Removes the copies of images no cached environment refers to anymore.
fn remove_unused_images(root: &Path) {
    let Ok(entries) = fs::read_dir(root.join("envs")) else { return };
    let used: HashSet<String> = entries
        .flatten()
        .filter_map(|e| read_cache(&e.path()))
        .map(|c| image_dir_name(&c.image_id))
        .collect();
    let Ok(images) = fs::read_dir(root.join("images")) else { return };
    for image in images.flatten() {
        if !used.contains(image.file_name().to_string_lossy().as_ref()) {
            let _ = fs::remove_dir_all(image.path());
        }
    }
}

#[derive(Debug, Default)]
struct CopyStats {
    files: usize,
    placeholders: usize,
    skipped_packages: Vec<String>,
}

static COPY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Copies the container directory `path` into `dest`. Bytecode is skipped and compiled
/// extensions become empty files: the server only needs to know they exist. The copy
/// lands in a temporary directory renamed at the end, so an interrupted copy is never
/// taken for a complete one.
fn copy_from_container(docker: &DockerExec, path: &str, dest: &Path, ext_suffixes: &[String], entries: Option<&Vec<String>>) -> Result<CopyStats, String> {
    let parent = dest.parent().ok_or("invalid destination")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(".ols-tmp-{}-{}", std::process::id(), COPY_COUNTER.fetch_add(1, Ordering::Relaxed)));
    let result = extract_container_dir(docker, path, &tmp, ext_suffixes, entries).and_then(|stats| {
        match fs::rename(&tmp, dest) {
            Ok(()) => Ok(stats),
            // Another server copied it meanwhile.
            Err(_) if dest.is_dir() => Ok(stats),
            Err(e) => Err(e.to_string()),
        }
    });
    let _ = fs::remove_dir_all(&tmp);
    result
}

/// Preferably `tar` in the container, which leaves out bytecode and the packages of
/// `NOT_COPIED_PACKAGES` before they are streamed (78 MB instead of 1.4 GB for an Odoo 18
/// image); `docker cp`, which streams everything, when the image has no usable `tar`.
fn extract_container_dir(docker: &DockerExec, path: &str, tmp: &Path, ext_suffixes: &[String], entries: Option<&Vec<String>>) -> Result<CopyStats, String> {
    fs::create_dir_all(tmp).map_err(|e| e.to_string())?;
    if let Some(entries) = entries {
        let (skipped, kept): (Vec<&String>, Vec<&String>) = entries
            .iter()
            .filter(|e| e.as_str() != "__pycache__")
            .partition(|e| NOT_COPIED_PACKAGES.contains(&e.as_str()));
        let skipped_packages: Vec<String> = skipped.into_iter().cloned().collect();
        if kept.is_empty() {
            return Ok(CopyStats { skipped_packages, ..Default::default() });
        }
        let mut args = vec![
            S!("exec"), docker.container.clone(), S!("tar"), S!("-C"), path.to_string(), S!("-cf"), S!("-"),
            S!("--exclude=__pycache__"), S!("--exclude=*.pyc"),
        ];
        // `./` keeps a name starting with `-` from being read as an option.
        args.extend(kept.iter().map(|e| format!("./{e}")));
        match stream_archive(&docker.cli, &args, tmp, ext_suffixes, false) {
            Ok(mut stats) => {
                stats.skipped_packages = skipped_packages;
                return Ok(stats);
            }
            Err(_) => {
                let _ = fs::remove_dir_all(tmp);
                fs::create_dir_all(tmp).map_err(|e| e.to_string())?;
            }
        }
    }
    let args = [S!("cp"), S!("-L"), format!("{}:{path}", docker.container), S!("-")];
    stream_archive(&docker.cli, &args, tmp, ext_suffixes, true)
}

/// Runs `cli args`, which writes a tar archive to its stdout, and extracts it into `dest`.
fn stream_archive(cli: &str, args: &[String], dest: &Path, ext_suffixes: &[String], under_dir_name: bool) -> Result<CopyStats, String> {
    let mut child = Command::new(cli)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {cli}: {e}"))?;
    let mut stderr = child.stderr.take().unwrap();
    let stderr_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let stats = extract_archive(child.stdout.take().unwrap(), dest, ext_suffixes, under_dir_name);
    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr = stderr_reader.join().unwrap_or_default();
    if !status.success() {
        return Err(format!("'{cli} {}' failed: {}", args.first().map(String::as_str).unwrap_or(""), stderr.trim()));
    }
    stats
}

/// Extracts an archive into `dest`. `under_dir_name`: the entries are under the copied
/// directory's name (`docker cp`), which is dropped. Any entry that would leave `dest`
/// is ignored, and symbolic links are not created: a link to a directory outside `dest`
/// would let a later entry of the image write through it.
fn extract_archive(reader: impl Read, dest: &Path, ext_suffixes: &[String], under_dir_name: bool) -> Result<CopyStats, String> {
    let mut stats = CopyStats::default();
    let mut archive = tar::Archive::new(reader);
    let strip = |path: &Path| -> Option<PathBuf> {
        let mut components = path.components().filter(|c| !matches!(c, Component::CurDir));
        if under_dir_name {
            components.next();
        }
        let rest: PathBuf = components.collect();
        rest.components().all(|c| matches!(c, Component::Normal(_))).then_some(rest)
    };
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let Some(rel) = entry.path().ok().and_then(|p| strip(&p)) else { continue };
        if rel.as_os_str().is_empty() {
            continue;
        }
        if let Some(top) = rel.components().next().and_then(|c| c.as_os_str().to_str())
            && NOT_COPIED_PACKAGES.contains(&top)
        {
            if !stats.skipped_packages.iter().any(|p| p == top) {
                stats.skipped_packages.push(top.to_string());
            }
            continue;
        }
        let name = rel.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if rel.components().any(|c| c.as_os_str() == "__pycache__") || name.ends_with(".pyc") || name.ends_with(".pyo") {
            continue;
        }
        let out = dest.join(&rel);
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if entry_type.is_file() {
            if ext_suffixes.iter().any(|suffix| name.ends_with(suffix.as_str())) {
                fs::File::create(&out).map_err(|e| e.to_string())?;
                stats.placeholders += 1;
            } else {
                // Written by hand rather than `Entry::unpack`: no permissions, owner or
                // times from the image.
                let mut file = fs::File::create(&out).map_err(|e| e.to_string())?;
                std::io::copy(&mut entry, &mut file).map_err(|e| e.to_string())?;
                stats.files += 1;
            }
        } else if entry_type.is_hard_link()
            && let Some(target) = entry.link_name().ok().flatten().and_then(|t| strip(&t))
        {
            let _ = fs::copy(dest.join(target), &out);
        }
    }
    Ok(stats)
}

/// Where the cached environments and container copies live.
pub fn default_cache_root() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("odoo-ls").join("python-env"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_commands() {
        assert_eq!(split_command("docker exec -i odoo python3"), words(&["docker", "exec", "-i", "odoo", "python3"]));
        assert_eq!(split_command(r#"  "/opt/my python/bin/python"  -X 'a b' c\ d "#), words(&["/opt/my python/bin/python", "-X", "a b", "c d"]));
        assert_eq!(split_command(""), Vec::<String>::new());
        assert_eq!(split_command("''"), words(&[""]));
    }

    #[test]
    fn parses_docker_exec() {
        let parse = |s: &str| DockerExec::parse(&split_command(s)).map(|d| (d.cli, d.container));
        assert_eq!(parse("docker exec -i odoo python3"), Some((S("docker"), S("odoo"))));
        assert_eq!(parse("/usr/bin/podman exec -it -u odoo --workdir /tmp -e A=1 --env=B=2 web python3 -I"), Some((S("/usr/bin/podman"), S("web"))));
        assert_eq!(parse("docker exec odoo"), None, "no interpreter after the container");
        assert_eq!(parse("docker run -i odoo python3"), None);
        assert_eq!(parse("python3"), None);
        assert_eq!(parse("/opt/docker-python/bin/python exec x y"), None);
    }

    #[allow(non_snake_case)]
    fn S(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn python_command_takes_precedence_over_python_path() {
        use crate::core::config::{ConfigEntry, ConfigKey};
        let mut config = ConfigEntry::new();
        config.set_str(ConfigKey::PythonPath, S("/usr/bin/python3"));
        assert_eq!(config.python_command(), words(&["/usr/bin/python3"]));
        config.set_str(ConfigKey::PythonCommand, S("docker exec -i odoo python3"));
        assert_eq!(config.python_command(), words(&["docker", "exec", "-i", "odoo", "python3"]));
        config.set_str(ConfigKey::PythonCommand, S("  "));
        assert_eq!(config.python_command(), words(&["/usr/bin/python3"]), "a blank command falls back to python_path");
    }

    #[test]
    fn maps_paths_by_longest_prefix() {
        let mappings = vec![
            PathMapping::parse("/usr/lib/python3=/host/py").unwrap(),
            PathMapping::parse("/usr/lib/python3/dist-packages/=/host/dist").unwrap(),
        ];
        assert_eq!(map_path("/usr/lib/python3/dist-packages", &mappings).as_deref(), Some("/host/dist"));
        assert_eq!(map_path("/usr/lib/python3/dist-packages/odoo", &mappings).as_deref(), Some("/host/dist/odoo"));
        assert_eq!(map_path("/usr/lib/python3/x", &mappings).as_deref(), Some("/host/py/x"));
        assert_eq!(map_path("/usr/lib/python312", &mappings), None, "prefix must end at a path separator");
        assert_eq!(PathMapping::parse("no-separator"), None);
        assert_eq!(PathMapping::parse("=/host"), None);
    }

    /// A fake `docker` CLI over the directory `ctr/`: `inspect` prints the image ID (or
    /// fails while `stopped` exists), `exec ... python3` runs the probe, `exec <c> tar`
    /// runs tar (or fails while `no_tar` exists) and `cp` streams a directory as tar.
    /// Each kind of call is counted in a file.
    #[cfg(unix)]
    fn fake_docker(root: &Path) -> String {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let script = r#"#!/bin/sh
D="$(dirname "$0")/.."
case "$1" in
  inspect) [ -f "$D/stopped" ] && { echo "Cannot connect to the Docker daemon" >&2; exit 1; }; cat "$D/image";;
  exec)
    if [ "$3" = tar ]; then
      echo x >> "$D/tar_calls"; [ -f "$D/no_tar" ] && exit 127
      p="$5"; shift 5; cd "$D/ctr$p" && exec tar "$@"
    fi
    echo x >> "$D/exec_calls"; echo "a wrapper banner"
    echo '{"version_info": [3, 12, 1], "ext_suffixes": [".cpython-312-x86_64-linux-gnu.so", ".abi3.so", ".so"], "sys_path": ["/usr/lib/py/site-packages", "/usr/lib/py/site-packages/nested", "/mnt/extra"], "entries": {"/usr/lib/py/site-packages": ["escape", "nested", "odoo", "pkg"]}}';;
  cp) echo x >> "$D/cp_calls"; src="${3#*:}"; tar -C "$D/ctr$(dirname "$src")" -cf - "$(basename "$src")";;
esac
"#;
        let docker = bin.join("docker");
        fs::write(&docker, script).unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o755)).unwrap();
        docker.to_string_lossy().into_owned()
    }

    fn calls(root: &Path, name: &str) -> usize {
        fs::read_to_string(root.join(name)).map(|s| s.lines().count()).unwrap_or(0)
    }

    #[cfg(unix)]
    #[test]
    fn docker_environment_is_probed_once_per_image() {
        let root = std::env::temp_dir().join(format!("ols-python-env-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let site = root.join("ctr/usr/lib/py/site-packages");
        fs::create_dir_all(site.join("pkg/__pycache__")).unwrap();
        fs::create_dir_all(site.join("nested")).unwrap();
        fs::write(site.join("pkg/__init__.py"), "x = 1\n").unwrap();
        fs::write(site.join("pkg/__pycache__/__init__.cpython-312.pyc"), "bytecode").unwrap();
        fs::write(site.join("pkg/_speedups.cpython-312-x86_64-linux-gnu.so"), "binary").unwrap();
        fs::write(site.join("nested/inner.py"), "").unwrap();
        // A link out of the copy: not created, a later entry could write through it.
        std::os::unix::fs::symlink(root.join("host"), site.join("escape")).unwrap();
        fs::create_dir_all(site.join("odoo")).unwrap();
        fs::write(site.join("odoo/__init__.py"), "").unwrap();
        let host_extra = root.join("host/extra");
        fs::create_dir_all(&host_extra).unwrap();
        fs::write(root.join("image"), "sha256:aaa111\n").unwrap();
        let cache = root.join("cache");
        let command = vec![fake_docker(&root), S("exec"), S("-i"), S("odoo"), S("python3")];
        let mappings = vec![PathMapping { container: S("/mnt/extra"), host: host_extra.to_string_lossy().into_owned() }];

        let counts = || (calls(&root, "exec_calls"), calls(&root, "tar_calls"), calls(&root, "cp_calls"));
        // Cold start: one probe, one copy with tar (the nested entry comes with its parent).
        let first = resolve(&command, &mappings, Some(&cache)).unwrap();
        assert!(!first.from_cache);
        assert_eq!(counts(), (1, 1, 0));
        assert_eq!(first.env.version, vec![3, 12, 1]);
        let copy = cache.join("images/aaa111/fs/usr/lib/py/site-packages");
        assert_eq!(first.env.sys_path, vec![
            copy.to_string_lossy().into_owned(),
            copy.join("nested").to_string_lossy().into_owned(),
            host_extra.to_string_lossy().into_owned(),
        ]);
        assert_eq!(fs::read_to_string(copy.join("pkg/__init__.py")).unwrap(), "x = 1\n");
        assert!(!copy.join("pkg/__pycache__").exists(), "bytecode is not copied");
        assert!(!copy.join("odoo").exists(), "Odoo comes from odoo_path");
        assert!(fs::symlink_metadata(copy.join("escape")).is_err(), "symbolic links are not created");
        assert_eq!(fs::metadata(copy.join("pkg/_speedups.cpython-312-x86_64-linux-gnu.so")).unwrap().len(), 0, "extensions become placeholders");

        // Same image: nothing runs in the container.
        let second = resolve(&command, &mappings, Some(&cache)).unwrap();
        assert!(second.from_cache);
        assert!(second.notes.iter().all(|n| !n.starts_with("copied")), "copies are not reported again: {:?}", second.notes);
        assert_eq!(second.env, first.env);
        assert_eq!(counts(), (1, 1, 0));

        // New image without tar: probed again, copied with `docker cp`, the old copy is removed.
        fs::write(root.join("image"), "sha256:bbb222\n").unwrap();
        fs::write(root.join("no_tar"), "").unwrap();
        let third = resolve(&command, &mappings, Some(&cache)).unwrap();
        assert!(!third.from_cache);
        assert_eq!(counts(), (2, 2, 1));
        let copy = cache.join("images/bbb222/fs/usr/lib/py/site-packages");
        assert!(copy.join("pkg/__init__.py").is_file());
        assert!(!copy.join("pkg/__pycache__").exists() && !copy.join("odoo").exists());
        assert!(!cache.join("images/aaa111").exists());

        // Docker unavailable: the cached environment is used, without waiting or failing.
        fs::write(root.join("stopped"), "").unwrap();
        let fourth = resolve(&command, &mappings, Some(&cache)).unwrap();
        assert!(fourth.from_cache);
        assert_eq!(fourth.env, third.env);
        assert!(fourth.notes.iter().any(|n| n.contains("using the environment cached")));
        assert_eq!(calls(&root, "exec_calls"), 2);

        // ... but without a cache there is nothing to fall back to.
        assert!(resolve(&command, &mappings, Some(&root.join("empty-cache"))).is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
