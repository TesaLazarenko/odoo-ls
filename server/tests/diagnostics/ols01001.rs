use std::env;

use lsp_types::{DiagnosticSeverity, NumberOrString};
use odoo_ls_server::{S, threads::SessionInfo, utils::PathSanitizer};

use crate::common::*;

pub fn test_ols01001(mut session: SessionInfo) {
    let path = env::current_dir().unwrap().join("tests/data/python/diagnostics/ols01001.py").sanitize();
    prepare_custom_entry_point(&mut session, &path);
    let diagnostics = get_diagnostics_for_path(&mut session, &path);
    let diagnostics = diag_on_line(&diagnostics, 1);
    assert_eq!(diagnostics.len(), 1);
    let diag = &diagnostics[0];
    assert!(diag.code.is_some());
    let code = match &diag.code {
        Some(NumberOrString::String(code)) => code,
        Some(NumberOrString::Number(num)) => panic!("Unexpected numeric code: {}", num),
        None => panic!("Diagnostic code is None"),
    };
    assert!(code == &S!("OLS01001"));
    assert!(diag.severity.is_some_and(|s| s == DiagnosticSeverity::WARNING));
}
/// An unresolved subscripted base is reported with its expression, a `Generic[...]` base is not reported.
pub fn test_ols01001_subscripted_base(mut session: SessionInfo) {
    let path = env::current_dir().unwrap().join("tests/data/python/diagnostics/ols01001_subscript.py").sanitize();
    prepare_custom_entry_point(&mut session, &path);
    let diagnostics = get_diagnostics_for_path(&mut session, &path);
    assert!(diag_on_line(&diagnostics, 4).is_empty(), "Generic[T] base: {:?}", diag_on_line(&diagnostics, 4));
    let unknown = diag_on_line(&diagnostics, 7);
    assert_eq!(unknown.len(), 1, "{diagnostics:?}");
    assert_eq!(unknown[0].code, Some(NumberOrString::String(S!("OLS01001"))));
    assert!(unknown[0].message.contains("Unknown[int]"), "{}", unknown[0].message);
    let missing = diag_on_line(&diagnostics, 10);
    assert_eq!(missing.len(), 1, "{diagnostics:?}");
    assert!(missing[0].message.contains("missing.Base[T]"), "{}", missing[0].message);
}
