#[path = "../common/mod.rs"]
mod common;

use std::path::PathBuf;

use lsp_types::{Diagnostic, DiagnosticSeverity, NumberOrString};
use odoo_ls_server::utils::PathSanitizer;

pub mod ols01000;
pub mod ols01001;
pub mod ols01002;
pub mod ols01003;
pub mod ols01004;
pub mod ols01005;
pub mod ols01006;
pub mod ols01007;
pub mod ols01008;
pub mod ols01009;
pub mod ols01010;
pub mod ols01011;
pub mod ols02001;
pub mod ols02002;
pub mod ols03000_1_to_23;
pub mod ols03009_operators;
pub mod ols03018_compute_sql;
pub mod ols03011_search_methods;
pub mod ols03014_record_values;
pub mod ols03025_to_27;
pub mod ols03028;
pub mod ols03029;
pub mod ols03301;
pub mod ols03302;
pub mod ols04000_1_to_20;
pub mod ols05000s;
pub mod ols05073_74;
pub mod implicit_class_method_no_ols_01007;
pub mod magic_fields_comodel;
pub mod panic_super_arg;
pub mod test_csv_diagnostics;
pub mod test_duplicate_template;

use crate::{
    implicit_class_method_no_ols_01007::implicit_class_method_no_ols_01007,
    magic_fields_comodel::test_magic_fields_comodel,
    ols01000::test_ols01000,
    ols01001::test_ols01001,
    ols01002::test_ols01002,
    ols01003::test_ols01003,
    ols01004::test_ols01004,
    ols01005::test_ols01005,
    ols01006::test_ols01006,
    ols01007::test_ols01007,
    ols01008::test_ols01008,
    ols01009::test_ols01009,
    ols01010::test_ols01010,
    ols01011::test_ols01011,
    ols02001::test_ols02001,
    ols02002::test_ols02002,
    ols03000_1_to_23::test_ols03001_23,
    ols03009_operators::test_ols03009_version_operators,
    ols03018_compute_sql::test_ols03018_compute_sql_method,
    ols03025_to_27::test_ols03025_to_27_access_operator,
    ols03028::test_ols03028_indirect_method_inheritance,
    ols03301::test_ols03301,
    ols03302::test_ols03302,
    ols04000_1_to_20::test_ols04001_to_20,
    ols05000s::{test_ols05000_2_3_py_file, test_ols05000s_manifest, test_ols05000s_xml_file},
    ols05073_74::{test_ols05073_05074_backend_template, test_ols05074_frontend_template},
    panic_super_arg::{
        test_super_first_arg_any_does_not_panic, test_super_first_arg_none_does_not_panic,
        test_super_first_arg_unbound_does_not_panic,
    },
    test_csv_diagnostics::test_csv_diagnostics,
    common::{build_once_run_isolated, create_init_session, diag_on_line, get_diagnostics_for_path, setup_server},
};

#[test]
fn test_diagnostics_with_odoo() {
    let (odoo, config) = setup_server(true);
    build_once_run_isolated(odoo, config, &[
        &test_ols03001_23,
        &test_ols03009_version_operators,
        &test_ols03018_compute_sql_method,
        &test_ols03025_to_27_access_operator,
        &test_ols03028_indirect_method_inheritance,
        &test_ols03301,
        &test_ols03302,
        &test_ols04001_to_20,
        &test_ols05000_2_3_py_file,
        &test_ols05000s_xml_file,
        &test_ols05000s_manifest,
        &test_ols05073_05074_backend_template,
        &test_ols05074_frontend_template,
        &test_magic_fields_comodel,
        &test_csv_diagnostics,
    ]);
}

/// Same as `test_diagnostics_isolated`, on a server built without Odoo.
#[test]
fn test_diagnostics_no_odoo() {
    let (odoo, config) = setup_server(false);
    build_once_run_isolated(odoo, config, &[
        &test_ols01000,
        &test_ols01001,
        &test_ols01002,
        &test_ols01003,
        &test_ols01004,
        &test_ols01005,
        &test_ols01006,
        &test_ols01007,
        &test_ols01008,
        &test_ols01009,
        &test_ols01010,
        &test_ols01011,
        &test_ols02001,
        &test_ols02002,
        &implicit_class_method_no_ols_01007,
        &test_super_first_arg_any_does_not_panic,
        &test_super_first_arg_none_does_not_panic,
        &test_super_first_arg_unbound_does_not_panic,
    ]);
}

/// Returns the failures of `cases` in `file` of `module_for_diagnostics/models`.
/// Each case is a statement as written, found by its line, then the expected code and the text it
/// points at if invalid, as the only Error of that line.
pub(crate) fn case_failures(file: &str, cases: &[(&str, Option<(&str, &str)>)]) -> Vec<String> {
    let (mut odoo, config) = setup_server(true);
    let mut session = create_init_session(&mut odoo, config);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("addons")
        .join("module_for_diagnostics")
        .join("models")
        .join(file)
        .sanitize();
    let diagnostics = get_diagnostics_for_path(&mut session, &path);
    let lines: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    let mut failures = vec![];
    for &(statement, expected) in cases {
        let Some(line) = lines.iter().position(|text| text.trim() == statement) else {
            failures.push(format!("{statement}: no such line in {file}"));
            continue;
        };
        let text = &lines[line];
        let found = diag_on_line(&diagnostics, line as u32);
        let as_expected = match expected {
            None => found.is_empty(),
            Some((code, pointed)) => matches!(found.as_slice(), [diagnostic]
                if matches!(&diagnostic.code, Some(NumberOrString::String(c)) if c == code)
                && diagnostic.severity == Some(DiagnosticSeverity::ERROR)
                && pointed_text(diagnostic, text) == Some(pointed)),
        };
        if !as_expected {
            let found: Vec<_> = found
                .iter()
                .map(|diagnostic| describe(diagnostic, text))
                .collect();
            failures.push(format!("{statement}: expected {expected:?}, got {found:?}"));
        }
    }
    failures
}

/// The text of `line` under a diagnostic, if the diagnostic is on that line only.
fn pointed_text<'a>(diagnostic: &Diagnostic, line: &'a str) -> Option<&'a str> {
    let range = diagnostic.range;
    if range.start.line != range.end.line {
        return None;
    }
    line.get(range.start.character as usize..range.end.character as usize)
}

fn describe(diagnostic: &Diagnostic, line: &str) -> String {
    let code = match &diagnostic.code {
        Some(NumberOrString::String(code)) => code.clone(),
        other => format!("{other:?}"),
    };
    format!(
        "{code} {:?} on {:?}: {}",
        diagnostic.severity,
        pointed_text(diagnostic, line),
        diagnostic.message
    )
}
