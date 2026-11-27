//! On-disk index of the Odoo modules found in the addons paths.
//!
//! For every module directory it records what the server needs to know about a module
//! *without loading it*: its manifest dependencies, its `auto_install` rule and the
//! models it declares (`_name = "..."` in a class body). It is persisted under the user
//! cache directory and revalidated per module from file sizes and modification times,
//! so after the first run only changed modules are scanned again.
//!
//! It serves two purposes:
//! * `modules_scope = "dependencies"`: compute the dependency closure of the workspace
//!   modules, so that only those modules are loaded in memory (see `SyncOdoo::build_modules`);
//! * knowing which module declares a model that is not loaded, so that a reference to it
//!   is still reported as "not in the dependencies of your module" (OLS03001) and not as an
//!   unknown model (OLS03002).
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use ruff_python_ast::{Expr, Stmt};
use rustc_hash::FxHasher;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// Bump when the format or the scanning rules change: older cache files are ignored.
const INDEX_FORMAT_VERSION: u32 = 3;
/// Directories never scanned for model declarations (same idea as the pre-parser).
const SKIPPED_DIRS: &[&str] = &[
    "__pycache__", "static", "node_modules", "i18n", "views", "data", "security",
    "description", "doc", "docs", "migrations", "upgrades",
];

/// When a module is installed automatically (Odoo `auto_install`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AutoInstall {
    #[default]
    No,
    /// `auto_install: True`: as soon as all of `depends` are installed.
    AllDepends,
    /// `auto_install: [...]`: as soon as these modules are installed.
    Modules(Vec<String>),
}

/// A model declared with `_name` in a module: where, so that a model of a module that is
/// not loaded can still be listed (workspace symbols) and opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexedModel {
    pub name: String,
    /// Absolute path of the Python file.
    pub file: String,
    /// 0-based line of the class that declares it.
    pub line: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleIndexEntry {
    pub name: String,
    pub path: String,
    pub depends: Vec<String>,
    pub auto_install: AutoInstall,
    /// Models declared with `_name` in the module's Python files.
    pub models: Vec<IndexedModel>,
    /// Fingerprint of the module's Python files and manifest (paths, sizes, mtimes).
    signature: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct IndexFile {
    version: u32,
    /// Keyed by module path.
    modules: BTreeMap<String, ModuleIndexEntry>,
}

#[derive(Debug, Default, Clone)]
pub struct ModuleIndex {
    /// Keyed by module (directory) name; the first addons path wins, as in Odoo.
    by_name: HashMap<String, ModuleIndexEntry>,
    by_model: HashMap<String, Vec<String>>,
    /// Where it is persisted: `None` for addons paths in the temporary directory (tests,
    /// throwaway checkouts), which would leave a new cache file behind every run.
    cache_path: Option<PathBuf>,
}

impl ModuleIndex {
    /// Load the index for `addons_paths` from the cache, rescanning the modules whose files
    /// changed, and write it back if anything changed.
    pub fn load_or_build(addons_paths: &[PathBuf]) -> Self {
        let cache_path = if addons_paths.iter().any(|p| is_temporary(p)) { None } else { cache_file_for(addons_paths) };
        let mut cached: IndexFile = cache_path.as_ref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .filter(|f: &IndexFile| f.version == INDEX_FORMAT_VERSION)
            .unwrap_or_default();
        let mut module_dirs = vec![];
        let mut seen_paths = HashSet::new();
        for addons_path in addons_paths.iter().filter(|p| seen_paths.insert(p.as_path())) {
            let Ok(entries) = fs::read_dir(addons_path) else { continue };
            let mut dirs: Vec<PathBuf> = entries.flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .filter(|p| p.join("__manifest__.py").is_file())
                .collect();
            dirs.sort();
            module_dirs.extend(dirs);
        }
        // Fingerprint every module, rescan the ones that changed (in parallel).
        let signatures: Vec<(PathBuf, u64)> = module_dirs.iter().map(|d| (d.clone(), module_signature(d))).collect();
        let to_scan: Vec<(PathBuf, u64)> = signatures.iter()
            .filter(|(dir, sig)| cached.modules.get(&path_key(dir)).is_none_or(|e| e.signature != *sig))
            .cloned()
            .collect();
        let rescanned = scan_modules_parallel(&to_scan);
        let changed = !rescanned.is_empty() || cached.modules.len() != module_dirs.len();
        for entry in rescanned {
            cached.modules.insert(entry.path.clone(), entry);
        }
        let present: HashSet<String> = module_dirs.iter().map(|d| path_key(d)).collect();
        cached.modules.retain(|path, _| present.contains(path));
        if changed && let Some(cache_path) = &cache_path {
            cached.version = INDEX_FORMAT_VERSION;
            if let Some(parent) = cache_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            // Write then rename, so that a concurrent server never reads a partial file.
            let tmp = cache_path.with_extension(format!("tmp{}", std::process::id()));
            match serde_json::to_vec(&cached).map(|bytes| fs::write(&tmp, bytes)) {
                Ok(Ok(())) => { let _ = fs::rename(&tmp, cache_path); },
                _ => warn!("Unable to write the module index to {}", cache_path.display()),
            }
        }
        info!("Module index: {} modules, {} rescanned", module_dirs.len(), to_scan.len());
        let mut index = ModuleIndex { cache_path, ..Default::default() };
        // Keep the addons-path order: the first module of a given name wins.
        for dir in &module_dirs {
            if let Some(entry) = cached.modules.get(&path_key(dir))
                && !index.by_name.contains_key(&entry.name) {
                    for model in &entry.models {
                        let modules = index.by_model.entry(model.name.clone()).or_default();
                        if !modules.contains(&entry.name) {
                            modules.push(entry.name.clone());
                        }
                    }
                    index.by_name.insert(entry.name.clone(), entry.clone());
            }
        }
        index
    }

    /// The file the index is persisted to, if any.
    pub fn cache_file(&self) -> Option<&Path> {
        self.cache_path.as_deref()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ModuleIndexEntry> {
        self.by_name.values()
    }

    pub fn get(&self, module: &str) -> Option<&ModuleIndexEntry> {
        self.by_name.get(module)
    }

    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Modules that declare `model` with `_name`, loaded or not.
    pub fn modules_declaring_model(&self, model: &str) -> &[String] {
        self.by_model.get(model).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Every model declared in the index, with the modules that declare it.
    pub fn models(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.by_model.iter().map(|(model, modules)| (model.as_str(), modules.as_slice()))
    }

    /// The module that contains `path` (a file or directory), if it is one of the index.
    /// Paths are compared canonically: the editor may open a file through a symlink.
    pub fn module_of_path(&self, path: &Path) -> Option<&ModuleIndexEntry> {
        let path = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        path.ancestors().find_map(|dir| {
            let entry = self.by_name.get(dir.file_name()?.to_str()?)?;
            let entry_path = Path::new(&entry.path);
            (entry_path == dir || fs::canonicalize(entry_path).is_ok_and(|p| p == dir)).then_some(entry)
        })
    }

    /// The modules Odoo would install for `roots`: their transitive dependencies, plus the
    /// `auto_install` modules that this set triggers (repeated until stable). `base` is
    /// always included. Unknown names are ignored.
    pub fn install_closure<'a>(&self, roots: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
        let mut closure: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<String> = roots.into_iter().map(str::to_string).collect();
        queue.push_back("base".to_string());
        loop {
            while let Some(name) = queue.pop_front() {
                if closure.contains(&name) {
                    continue;
                }
                let Some(entry) = self.by_name.get(&name) else { continue };
                closure.insert(name);
                queue.extend(entry.depends.iter().cloned());
            }
            // auto_install modules whose trigger is now satisfied
            let mut added = false;
            for entry in self.by_name.values() {
                if closure.contains(&entry.name) {
                    continue;
                }
                let triggers: &[String] = match &entry.auto_install {
                    AutoInstall::No => continue,
                    AutoInstall::AllDepends => &entry.depends,
                    AutoInstall::Modules(m) => m,
                };
                if triggers.iter().all(|t| closure.contains(t)) {
                    queue.push_back(entry.name.clone());
                    added = true;
                }
            }
            if !added {
                return closure;
            }
        }
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn is_temporary(path: &Path) -> bool {
    let temp = std::env::temp_dir();
    let temp = fs::canonicalize(&temp).unwrap_or(temp);
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()).starts_with(temp)
}

fn cache_file_for(addons_paths: &[PathBuf]) -> Option<PathBuf> {
    let mut hasher = FxHasher::default();
    for p in addons_paths {
        p.hash(&mut hasher);
    }
    let dir = dirs::cache_dir()?.join("odoo-ls").join("module-index");
    Some(dir.join(format!("{:016x}.json", hasher.finish())))
}

/// Walk the module's Python files (same skip rules as the scan) and the manifest.
fn module_py_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                let name = e.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with('.') && !SKIPPED_DIRS.contains(&name.as_ref()) {
                    stack.push(p);
                }
            } else if p.extension().is_some_and(|x| x == "py") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn module_signature(dir: &Path) -> u64 {
    let mut hasher = FxHasher::default();
    INDEX_FORMAT_VERSION.hash(&mut hasher);
    for f in module_py_files(dir) {
        f.hash(&mut hasher);
        if let Ok(meta) = fs::metadata(&f) {
            meta.len().hash(&mut hasher);
            if let Ok(m) = meta.modified() && let Ok(d) = m.duration_since(UNIX_EPOCH) {
                d.as_nanos().hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

fn scan_modules_parallel(to_scan: &[(PathBuf, u64)]) -> Vec<ModuleIndexEntry> {
    if to_scan.is_empty() {
        return vec![];
    }
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8);
    let chunk = to_scan.len().div_ceil(n);
    std::thread::scope(|scope| {
        let handles: Vec<_> = to_scan.chunks(chunk.max(1))
            .map(|part| scope.spawn(move || part.iter().filter_map(|(dir, sig)| scan_module(dir, *sig)).collect::<Vec<_>>()))
            .collect();
        handles.into_iter().flat_map(|h| h.join().unwrap_or_default()).collect()
    })
}

fn scan_module(dir: &Path, signature: u64) -> Option<ModuleIndexEntry> {
    let name = dir.file_name()?.to_string_lossy().to_string();
    let manifest = fs::read_to_string(dir.join("__manifest__.py")).ok()?;
    let (depends, auto_install) = parse_manifest(&manifest);
    let mut models = vec![];
    for file in module_py_files(dir) {
        if file.file_name().is_some_and(|n| n == "__manifest__.py") {
            continue;
        }
        let Ok(source) = fs::read_to_string(&file) else { continue };
        // Cheap pre-filter: most files declare no model.
        if !source.contains("_name") {
            continue;
        }
        let parsed = ruff_python_parser::parse_module(&source);
        let Ok(parsed) = parsed else { continue };
        let mut names = vec![];
        collect_model_names(parsed.suite(), &mut names);
        let file_path = path_key(&file);
        for (name, offset) in names {
            let line = source.as_bytes()[..offset.min(source.len())].iter().filter(|&&b| b == b'\n').count() as u32;
            models.push(IndexedModel { name, file: file_path.clone(), line });
        }
    }
    models.sort_by(|a, b| (&a.name, &a.file, a.line).cmp(&(&b.name, &b.file, b.line)));
    models.dedup();
    Some(ModuleIndexEntry { name, path: path_key(dir), depends, auto_install, models, signature })
}

fn parse_manifest(source: &str) -> (Vec<String>, AutoInstall) {
    let mut depends = vec![];
    let mut auto_install = AutoInstall::No;
    let mut country_specific = false;
    let Ok(parsed) = ruff_python_parser::parse_module(source) else { return (depends, auto_install) };
    let Some(Stmt::Expr(e)) = parsed.suite().first() else { return (depends, auto_install) };
    let Expr::Dict(dict) = &*e.value else { return (depends, auto_install) };
    for item in dict.items.iter() {
        let Some(Expr::StringLiteral(key)) = item.key.as_ref() else { continue };
        match key.value.to_str() {
            "depends" => depends = string_list(&item.value),
            "auto_install" => auto_install = match &item.value {
                Expr::BooleanLiteral(b) if b.value => AutoInstall::AllDepends,
                Expr::List(_) | Expr::Tuple(_) => AutoInstall::Modules(string_list(&item.value)),
                _ => AutoInstall::No,
            },
            // Localizations (`countries`) are auto-installed only for a company of one of
            // these countries: not something the workspace implies.
            "countries" => country_specific = !string_list(&item.value).is_empty(),
            _ => {}
        }
    }
    if country_specific {
        auto_install = AutoInstall::No;
    }
    (depends, auto_install)
}

fn string_list(expr: &Expr) -> Vec<String> {
    let elts = match expr {
        Expr::List(l) => &l.elts,
        Expr::Tuple(t) => &t.elts,
        _ => return vec![],
    };
    elts.iter().filter_map(|e| match e {
        Expr::StringLiteral(s) => Some(s.value.to_str().to_string()),
        _ => None,
    }).collect()
}

/// `_name = "model"` (or `_name: str = "model"`) directly in a class body, at any depth,
/// with the offset of the class.
fn collect_model_names(stmts: &[Stmt], out: &mut Vec<(String, usize)>) {
    for stmt in stmts {
        match stmt {
            Stmt::ClassDef(class) => {
                for s in class.body.iter() {
                    let value = match s {
                        Stmt::Assign(a) if a.targets.len() == 1 && matches!(&a.targets[0], Expr::Name(n) if n.id.as_str() == "_name") => Some(&*a.value),
                        Stmt::AnnAssign(a) if matches!(&*a.target, Expr::Name(n) if n.id.as_str() == "_name") => a.value.as_deref(),
                        _ => None,
                    };
                    if let Some(Expr::StringLiteral(s)) = value {
                        out.push((s.value.to_str().to_string(), class.range.start().to_usize()));
                    }
                }
                collect_model_names(&class.body, out);
            }
            Stmt::If(i) => {
                collect_model_names(&i.body, out);
                for clause in i.elif_else_clauses.iter() {
                    collect_model_names(&clause.body, out);
                }
            }
            Stmt::Try(t) => collect_model_names(&t.body, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_and_models_are_extracted() {
        let (deps, auto) = parse_manifest("{'name': 'x', 'depends': ['base', 'mail'], 'auto_install': True}");
        assert_eq!(deps, vec!["base", "mail"]);
        assert_eq!(auto, AutoInstall::AllDepends);
        let (_, auto) = parse_manifest("{'depends': ['a', 'b'], 'auto_install': ['a']}");
        assert_eq!(auto, AutoInstall::Modules(vec!["a".to_string()]));
        let (_, auto) = parse_manifest("{'depends': ['account'], 'auto_install': ['account'], 'countries': ['be']}");
        assert_eq!(auto, AutoInstall::No);
        let parsed = ruff_python_parser::parse_module("class A(models.Model):\n    _name = 'a.b'\n    _inherit = 'c'\nclass B:\n    _name: str = 'd'\n").unwrap();
        let mut models = vec![];
        collect_model_names(parsed.suite(), &mut models);
        assert_eq!(models, vec![("a.b".to_string(), 0), ("d".to_string(), 60)]);
    }

    #[test]
    fn closure_follows_depends_and_auto_install() {
        let mut index = ModuleIndex::default();
        let mk = |name: &str, deps: &[&str], auto: AutoInstall| ModuleIndexEntry {
            name: name.to_string(), path: name.to_string(), depends: deps.iter().map(|s| s.to_string()).collect(),
            auto_install: auto, models: vec![], signature: 0,
        };
        for e in [
            mk("base", &[], AutoInstall::No),
            mk("sale", &["base"], AutoInstall::No),
            mk("stock", &["base"], AutoInstall::No),
            mk("sale_stock", &["sale", "stock"], AutoInstall::AllDepends),
            mk("sale_extra", &["sale"], AutoInstall::AllDepends),
            mk("crm", &["base"], AutoInstall::No),
        ] {
            index.by_name.insert(e.name.clone(), e);
        }
        let c = index.install_closure(["sale"]);
        assert!(c.contains("sale") && c.contains("base") && c.contains("sale_extra"));
        assert!(!c.contains("stock") && !c.contains("sale_stock") && !c.contains("crm"));
        let c = index.install_closure(["sale", "stock"]);
        assert!(c.contains("sale_stock"));
    }
}
