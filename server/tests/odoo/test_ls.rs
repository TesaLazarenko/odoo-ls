

use std::path::Path;
use std::{env, fs};
use lsp_types::{NumberOrString, TextDocumentContentChangeEvent};
use odoo_ls_server::core::file_mgr::FileMgr;
use odoo_ls_server::core::odoo::{Odoo, SyncOdoo};
use odoo_ls_server::threads::SessionInfo;
use odoo_ls_server::S;
use odoo_ls_server::utils::PathSanitizer;

pub fn test_structure(session: SessionInfo) {
    let odoo = &*session.sync_odoo;
    let st = &odoo.symbol_table;

    let odoo_path = env::var("COMMUNITY_PATH").unwrap();
    let odoo_path = Path::new(&odoo_path).sanitize();
    let odoo_path = odoo_path.as_str();
    assert!(!odoo.get_symbol(odoo_path, (&["odoo"], &[]), u32::MAX).is_empty());
    assert!(!odoo.get_symbol(odoo_path, (&["odoo", "addons"], &[]), u32::MAX).is_empty());
    assert!(!odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1"], &[]), u32::MAX).is_empty());
    assert!(!odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_2"], &[]), u32::MAX).is_empty());
    assert!(odoo.get_symbol(odoo_path, (&["odoo", "addons", "not_a_module"], &[]), u32::MAX).is_empty());

    assert!(odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "not_loaded"], &[]), u32::MAX).is_empty());
    assert!(odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "not_loaded", "not_loaded_file"], &[]), u32::MAX).is_empty());
    assert!(odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "not_loaded", "not_loaded_file"], &["NotLoadedClass"]), u32::MAX).is_empty());
    assert!(odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "not_loaded", "not_loaded_file"], &["NotLoadedFunc"]), u32::MAX).is_empty());

    let models = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "models"], &[]), u32::MAX);
    assert!(models.len() == 1);
    assert!(st.get_symbol(models[0], (&["base_test_models"], &[]), u32::MAX).len() == 1);
    assert!(st.get_symbol(models[0], (&[], &["base_test_models"]), u32::MAX).len() == 1);
    assert!(st.get_symbol(models[0], (&["base_test_models"], &[]), u32::MAX)[0] !=
            st.get_symbol(models[0], (&[], &["base_test_models"]), u32::MAX)[0]);
    let module_1 = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1"], &[]), u32::MAX);
    assert!(module_1.len() == 1);
    test_imports(odoo);
}

fn reload(session: &mut SessionInfo, path: &str, text: String, version: i32) {
    let event = [TextDocumentContentChangeEvent { range: None, range_length: None, text }];
    FileMgr::update_file_info(session, path, Some(event.as_slice()), Some(version), false);
    Odoo::update_file_index(session, Path::new(path), "py", false, false);
}

/// On its own server: reloading files may delay the rebuild, which needs the channel of a full session.
#[test]
fn test_imports_after_dependency_edit() {
    let (mut odoo, config) = crate::common::setup_server(true);
    let mut session = crate::common::create_init_session(&mut odoo, config);
    let odoo_path = Path::new(&env::var("COMMUNITY_PATH").unwrap()).sanitize();
    let addons = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/addons");
    let module_a = session.sync_odoo.get_symbol(&odoo_path, (&["odoo", "addons", "module_indirect_a"], &[]), u32::MAX)[0];
    let module_a_symbols = session.st().all_symbols(module_a).len();
    // star-imported by its package, which is star-imported by the parent package
    let constants = addons.join("module_1/constants/data/constants.py").sanitize();
    reload(&mut session, &constants, fs::read_to_string(&constants).unwrap() + "\n", 2);
    // module_indirect_a depends on module_indirect_base
    let manifest = addons.join("module_indirect_base/__manifest__.py").sanitize();
    reload(&mut session, &manifest, fs::read_to_string(&manifest).unwrap() + "\n", 2);
    test_imports(session.sync_odoo);
    assert_eq!(session.st().all_symbols(module_a).len(), module_a_symbols);
    let models_var = session.st().get_symbol(module_a, (&[], &["models"]), u32::MAX);
    assert_eq!(session.st().evaluations(models_var[0]).unwrap().len(), 1);
}

/// On its own server: reloading files may delay the rebuild, which needs the channel of a full session.
#[test]
fn test_import_found_again_after_rename() {
    let (mut odoo, config) = crate::common::setup_server(true);
    let mut session = crate::common::create_init_session(&mut odoo, config);
    let addons = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/addons");
    let constants = addons.join("module_1/constants/data/constants.py").sanitize();
    let data_init = addons.join("module_1/constants/data/__init__.py").sanitize();
    let importer = addons.join("module_1/models/base_test_models.py").sanitize();
    let has_ols02001 = |session: &mut SessionInfo| {
        let diagnostics = crate::common::get_diagnostics_for_paths(session, std::slice::from_ref(&importer));
        diagnostics.get(&importer).map(|d| d.iter().any(|d| d.code == Some(NumberOrString::String(S!("OLS02001")))))
    };
    // CONSTANT_2 is also defined in the package, so it would keep the dependency alive
    let importer_source = fs::read_to_string(&importer).unwrap();
    reload(&mut session, &importer, importer_source.replace("CONSTANT_1, CONSTANT_2", "CONSTANT_1"), 2);
    let source = fs::read_to_string(&constants).unwrap();
    reload(&mut session, &constants, source.replace("CONSTANT_1 = 1", "RENAMED = 1"), 2);
    assert_eq!(has_ols02001(&mut session), Some(true));
    reload(&mut session, &constants, source, 3);
    assert_eq!(has_ols02001(&mut session), Some(false));
    // missing names in the package itself and in its parent, which star-imports it
    let data_source = fs::read_to_string(&data_init).unwrap();
    reload(&mut session, &data_init, data_source.clone() + "\nfrom . import missing\nfrom .. import missing\n", 2);
    reload(&mut session, &data_init, data_source, 3);
    test_imports(session.sync_odoo);
}

fn test_imports(odoo: &SyncOdoo) {
    //test direct imports
    let st = &odoo.symbol_table;
    let odoo_path = env::var("COMMUNITY_PATH").unwrap();
    let odoo_path = Path::new(&odoo_path).sanitize();
    let odoo_path = odoo_path.as_str();
    let model_var = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1"], &["models"]), u32::MAX);
    let model_dir = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "models"], &[]), u32::MAX);
    assert!(model_var.len() == 1);
    assert!(model_dir.len() == 1);
    assert!(model_dir[0] != model_var[0]);
    assert!(st.evaluations(model_var[0]).as_ref().unwrap().len() == 1);
    assert!(st.evaluations(model_var[0]).as_ref().unwrap()[0].symbol.get_symbol_ptr().upgrade_weak(st) == Some(model_dir[0]));
    let data_var = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1"], &["data"]), u32::MAX);
    assert!(data_var.len() == 1);
    assert!(st.evaluations(data_var[0]).as_ref().unwrap().is_empty());

    //test * imports
    let constants_dir = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "constants"], &[]), u32::MAX);
    assert!(constants_dir.len() == 1);
    let constants_dir = constants_dir[0];
    assert!(st.all_symbols(constants_dir).len() == 3);
    assert!(st.get_symbol(constants_dir, (&[], &["CONSTANT_1"]), u32::MAX).len() == 1);
    assert!(st.get_symbol(constants_dir, (&[], &["CONSTANT_2"]), u32::MAX).len() == 1);
    assert!(st.get_symbol(constants_dir, (&[], &["CONSTANT_3"]), u32::MAX).is_empty());
    assert!(st.get_symbol(constants_dir, (&["data"], &[]), u32::MAX).len() == 1);
    assert!(st.evaluations(st.get_symbol(constants_dir, (&[], &["CONSTANT_1"]), u32::MAX)[0]).as_ref().unwrap().len() == 1);
    assert!(st.evaluations(st.get_symbol(constants_dir, (&[], &["CONSTANT_1"]), u32::MAX)[0]).as_ref().unwrap()[0].value.is_none());
    assert!(st.evaluations(st.get_symbol(constants_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap().len() == 1);
    assert!(st.evaluations(st.get_symbol(constants_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap()[0].value.is_none());
    assert!(!st.evaluations(st.get_symbol(constants_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap()[0].symbol.get_weak().weak.is_expired(st));
    let data_dir = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "constants", "data"], &[]), u32::MAX);
    assert!(data_dir.len() == 1);
    let data_dir = data_dir[0];
    assert!(st.all_symbols(data_dir).len() == 4);
    assert!(st.get_symbol(data_dir, (&[], &["CONSTANT_1"]), u32::MAX).len() == 1);
    assert!(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), u32::MAX).len() == 1);
    assert!(st.get_symbol(data_dir, (&[], &["CONSTANT_3"]), u32::MAX).is_empty());
    assert!(st.get_symbol(data_dir, (&["constants"], &[]), u32::MAX).len() == 1);
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_1"]), u32::MAX)[0]).as_ref().unwrap().len() == 1);
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_1"]), u32::MAX)[0]).as_ref().unwrap()[0].value.is_none());
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap().len() == 1);
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap()[0].value.is_some());
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap()[0].value.as_ref().unwrap().as_constant().is_number_literal_expr());
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), u32::MAX)[0]).as_ref().unwrap()[0].value.as_ref().unwrap().as_constant().as_number_literal_expr().unwrap().value.as_int().unwrap().as_i32().unwrap() == 22);
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), 26)[0]).as_ref().unwrap().len() == 1);
    assert!(st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), 26)[0]).as_ref().unwrap()[0].value.is_none());
    assert!(!st.evaluations(st.get_symbol(data_dir, (&[], &["CONSTANT_2"]), 26)[0]).as_ref().unwrap()[0].symbol.get_weak().weak.is_expired(st));

    //Test odoo.addons import
    let constant_1_var = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "models", "base_test_models"], &["CONSTANT_1"]), u32::MAX);
    assert!(constant_1_var.len() == 1);
    assert!(st.evaluations(constant_1_var[0]).as_ref().unwrap().len() == 1);
    let constant_1_var_data = odoo.get_symbol(odoo_path, (&["odoo", "addons", "module_1", "constants"], &["CONSTANT_1"]), u32::MAX);
    assert!(constant_1_var_data.len() == 1);
    assert!(st.evaluations(constant_1_var[0]).as_ref().unwrap()[0].symbol.get_symbol_ptr().upgrade_weak(st) == Some(constant_1_var_data[0]));

}
