//! `modules_scope = "dependencies"`: only the dependency closure of the workspace modules is
//! loaded at startup, and the other modules of the addons paths are loaded on demand.
//!
//! Fixture (in a temp dir, two addons paths besides Odoo):
//! * `lib/lib_a`, `lib/lib_b`, `lib/lib_c`, `lib/lib_d`: plain modules depending on `base`, each
//!   declaring a model;
//! * `lib/lib_ab`: `auto_install` module depending on `lib_a` and `lib_b`;
//! * `ws/ws_mod` (the workspace): depends on `lib_a`;
//! * `ws2/ws_other` (another folder, added to the workspace later): depends on `lib_c`.

use std::fs;
use std::path::{Path, PathBuf};

use assert_fs::TempDir;
use lsp_types::{
    CompletionResponse, DidChangeTextDocumentParams, DidChangeWorkspaceFoldersParams, DidOpenTextDocumentParams,
    TextDocumentContentChangeEvent, TextDocumentItem, VersionedTextDocumentIdentifier, WorkspaceFolder,
    WorkspaceFoldersChangeEvent, WorkspaceSymbolResponse,
};
use odoo_ls_server::S;
use odoo_ls_server::core::config::{ConfigEntry, ConfigKey};
use odoo_ls_server::core::file_mgr::FileMgr;
use odoo_ls_server::core::odoo::{Odoo, SyncOdoo};
use odoo_ls_server::features::completion::CompletionFeature;
use odoo_ls_server::features::workspace_symbols::WorkspaceSymbolFeature;
use odoo_ls_server::threads::SessionInfo;
use odoo_ls_server::utils::PathSanitizer;

struct Fixture {
    _temp: TempDir,
    lib: PathBuf,
    ws: PathBuf,
    ws2: PathBuf,
}

fn write_module(root: &Path, name: &str, manifest: &str, models: &str) {
    let dir = root.join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("__manifest__.py"), manifest).unwrap();
    fs::write(dir.join("__init__.py"), "from . import models\n").unwrap();
    fs::write(dir.join("models.py"), models).unwrap();
}

fn model(name: &str) -> String {
    format!("from odoo import fields, models\n\n\nclass M(models.Model):\n    _name = '{name}'\n    _description = '{name}'\n\n    name = fields.Char()\n")
}

const WS_MOD_MODELS: &str = "from odoo import models\n\n\nclass W(models.Model):\n    _inherit = 'lib.a'\n\n    def f(self):\n        return self.env['lib.']\n";

fn fixture() -> Fixture {
    let temp = TempDir::new().unwrap();
    // canonical: on macOS the temp dir is behind a symlink
    let root = fs::canonicalize(temp.path()).unwrap();
    let (lib, ws, ws2) = (root.join("lib"), root.join("ws"), root.join("ws2"));
    write_module(&lib, "lib_a", "{'name': 'lib_a', 'depends': ['base']}\n", &model("lib.a"));
    write_module(&lib, "lib_b", "{'name': 'lib_b', 'depends': ['base']}\n", &model("lib.b"));
    write_module(&lib, "lib_c", "{'name': 'lib_c', 'depends': ['base']}\n", &model("lib.c"));
    write_module(&lib, "lib_d", "{'name': 'lib_d', 'depends': ['base']}\n", &model("lib.d"));
    write_module(&lib, "lib_ab", "{'name': 'lib_ab', 'depends': ['lib_a', 'lib_b'], 'auto_install': True}\n",
        "from odoo import fields, models\n\n\nclass A(models.Model):\n    _inherit = 'lib.a'\n\n    x_ab = fields.Char()\n");
    write_module(&ws, "ws_mod", "{'name': 'ws_mod', 'depends': ['lib_a']}\n", WS_MOD_MODELS);
    write_module(&ws2, "ws_other", "{'name': 'ws_other', 'depends': ['lib_c']}\n",
        "from odoo import models\n\n\nclass C(models.Model):\n    _inherit = 'lib.c'\n");
    Fixture { _temp: temp, lib, ws, ws2 }
}

/// Server configured with the fixture's addons paths and only `ws` in the workspace.
fn server(fx: &Fixture) -> (SyncOdoo, ConfigEntry) {
    let (mut odoo, mut config) = crate::common::setup_server(true);
    config.set_string_list(ConfigKey::AddonsPaths, [fx.lib.sanitize(), fx.ws.sanitize(), fx.ws2.sanitize()]);
    config.set_str(ConfigKey::ModulesScope, S!("dependencies"));
    {
        let file_mgr = &mut odoo.file_mgr;
        let test_addons = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("data").join("addons");
        file_mgr.remove_workspace_folder(S!("test_addons_path"), FileMgr::pathname2uri(&test_addons.sanitize()));
        file_mgr.add_workspace_folder(S!("ws"), FileMgr::pathname2uri(&fx.ws.sanitize()));
    }
    (odoo, config)
}

fn loaded(session: &SessionInfo, name: &str) -> bool {
    let addons = session.sync_odoo.addons_namespace().expect("odoo/addons namespace should exist");
    SyncOdoo::is_module_loaded(session, addons, name)
}

fn open(session: &mut SessionInfo, path: &Path, text: &str) {
    Odoo::handle_did_open(session, DidOpenTextDocumentParams {
        text_document: TextDocumentItem {
            uri: FileMgr::pathname2uri(&path.sanitize()),
            language_id: "python".to_string(),
            version: 1,
            text: text.to_string(),
        },
    });
}

#[test]
fn test_modules_scope_dependencies() {
    let fx = fixture();
    let (mut odoo, config) = server(&fx);
    // A file of lib_d is open in the editor before the indexing ends.
    odoo.opened_files.push(fx.lib.join("lib_d").join("models.py").sanitize());
    let mut session = crate::common::create_init_session(&mut odoo, config);

    // Startup: the workspace module and its dependencies only, and the module of the open file.
    assert!(loaded(&session, "ws_mod") && loaded(&session, "lib_a") && loaded(&session, "base"));
    assert!(loaded(&session, "lib_d"), "lib_d has a file open in the editor and should be loaded once indexing is done");
    for name in ["lib_b", "lib_c", "lib_ab", "ws_other"] {
        assert!(!loaded(&session, name), "{name} is not a dependency of the workspace and should not be loaded");
    }

    unloaded_models_are_offered(&mut session, &fx);
    opening_a_file_loads_its_module(&mut session, &fx);
    new_dependency_loads_triggered_auto_install(&mut session, &fx);
    added_workspace_folder_loads_its_closure(&mut session, &fx);
    // Addons paths in the temp dir: the index is not persisted (no file left in the user cache).
    assert!(session.sync_odoo.module_index.as_ref().is_some_and(|i| i.cache_file().is_none()));
}

/// Workspace symbols and model name completion list the models of the unloaded modules.
fn unloaded_models_are_offered(session: &mut SessionInfo, fx: &Fixture) {
    let response = WorkspaceSymbolFeature::get_workspace_symbols(session, "lib.c".to_string()).unwrap();
    let Some(WorkspaceSymbolResponse::Nested(symbols)) = response else { panic!("expected nested workspace symbols") };
    let lib_c = symbols.iter().find(|s| s.name == "\"lib.c\"").expect("the model of the unloaded lib_c should be listed");
    let lsp_types::OneOf::Left(location) = &lib_c.location else { panic!("expected a full location") };
    assert_eq!(FileMgr::uri2pathname(location.uri.as_str()), fx.lib.join("lib_c").join("models.py").sanitize());
    assert_eq!(location.range.start.line, 3, "line of `class M`");
    assert_eq!(lib_c.container_name.as_deref(), Some("lib_c (module not loaded)"));

    // `self.env['lib.']` in ws_mod: lib.a is loaded, lib.b and lib.c come from the index.
    let path = fx.ws.join("ws_mod").join("models.py");
    open(session, &path, WS_MOD_MODELS);
    let file_info = session.file_mgr().get_file_info(&path.sanitize()).unwrap();
    let file_symbol = SyncOdoo::get_symbol_of_opened_file(session, &path).expect("ws_mod/models.py should be loaded");
    let response = CompletionFeature::autocomplete(session, file_symbol, file_info, None, 7, 29);
    let items = match response {
        Some(CompletionResponse::Array(items)) => items,
        Some(CompletionResponse::List(list)) => list.items,
        None => vec![],
    };
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"lib.a"), "loaded model expected, got {labels:?}");
    let lib_c = items.iter().find(|i| i.label == "lib.c").unwrap_or_else(|| panic!("unloaded model expected, got {labels:?}"));
    assert_eq!(lib_c.label_details.as_ref().and_then(|d| d.description.as_deref()), Some("require lib_c"));
}

/// Opening a file of a module that is not loaded loads the module, instead of making the file
/// a custom entry point without module context.
fn opening_a_file_loads_its_module(session: &mut SessionInfo, fx: &Fixture) {
    let path = fx.lib.join("lib_c").join("models.py");
    open(session, &path, &model("lib.c"));
    assert!(loaded(session, "lib_c"), "opening a file of lib_c should load lib_c");
    assert!(session.sync_odoo.entry_point_mgr.custom_entry_points().is_empty(), "no custom entry point expected");
    assert!(SyncOdoo::get_symbol_of_opened_file(session, &path).is_some(), "the file should be part of the module");
    assert!(session.model_mgr().get_model_key("lib.c").is_some_and(|model| session.model_mgr()[model].has_symbols(session.st())));
}

/// A new dependency is loaded at once (upstream behavior), and so are the `auto_install`
/// modules it triggers, as Odoo would install them.
fn new_dependency_loads_triggered_auto_install(session: &mut SessionInfo, fx: &Fixture) {
    let manifest = fx.ws.join("ws_mod").join("__manifest__.py");
    let uri = FileMgr::pathname2uri(&manifest.sanitize());
    open(session, &manifest, "{'name': 'ws_mod', 'depends': ['lib_a']}\n");
    Odoo::handle_did_change(session, DidChangeTextDocumentParams {
        text_document: VersionedTextDocumentIdentifier { uri, version: 2 },
        content_changes: vec![TextDocumentContentChangeEvent {
            range: None, range_length: None, text: "{'name': 'ws_mod', 'depends': ['lib_a', 'lib_b']}\n".to_string(),
        }],
    });
    assert!(loaded(session, "lib_b"), "the new dependency should be loaded");
    assert!(loaded(session, "lib_ab"), "lib_ab (auto_install on lib_a + lib_b) should be loaded");
}

/// A folder added to the workspace gets its modules and their dependencies loaded.
fn added_workspace_folder_loads_its_closure(session: &mut SessionInfo, fx: &Fixture) {
    assert!(!loaded(session, "ws_other"));
    Odoo::handle_did_change_workspace_folders(session, DidChangeWorkspaceFoldersParams {
        event: WorkspaceFoldersChangeEvent {
            added: vec![WorkspaceFolder { uri: FileMgr::pathname2uri(&fx.ws2.sanitize()), name: S!("ws2") }],
            removed: vec![],
        },
    });
    assert!(loaded(session, "ws_other"), "the module of the added workspace folder should be loaded");
}
