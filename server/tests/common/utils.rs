use odoo_ls_server::{
    core::{
        evaluation::Evaluation, file_mgr::FileInfoKey, symbols::{storage::SymbolTable, symbol_keys::{SourceFileKey, SymbolKey}},
    }, features::ast_utils::AstUtils, odoo_version::OdooVersion, threads::SessionInfo
};
use odoo_ls_server::constants::BuildSteps;
use odoo_ls_server::core::build_scheduler::BuildScheduler;
use std::sync::LazyLock;


/// Cursor position right after the single occurrence of `needle` in `content`.
pub fn position_after(content: &str, needle: &str) -> lsp_types::Position {
    let start = content.find(needle).unwrap_or_else(|| panic!("{needle:?} not found in the fixture"));
    assert!(content[start + 1..].find(needle).is_none(), "{needle:?} is not unique in the fixture");
    let offset = start + needle.len();
    let line = content[..offset].matches('\n').count();
    let character = offset - content[..offset].rfind('\n').map(|index| index + 1).unwrap_or(0);
    lsp_types::Position::new(line as u32, character as u32)
}

/// Whether the Odoo sources the tests run on are older than `min`, for a test written on the structure of
/// newer versions (`odoo.orm`, `odoo.init`, a version set on the session that needs them): logs that it is skipped.
pub fn skip_before(session: &SessionInfo, min: (u32, u32), test: &str) -> bool {
    let skip = session.sync_odoo.version < min;
    if skip {
        tracing::warn!("Skipping {test}: it needs the Odoo sources of {}.{} or later, got {:?}", min.0, min.1, session.sync_odoo.version);
    }
    skip
}

/// Returns the correct class name for CountryState/ResCountryState depending on Odoo version
pub static COUNTRY_STATE_CLASS_NAME: LazyLock<fn(OdooVersion) -> &'static str> = LazyLock::new(|| {
    |version: OdooVersion| {
        if version >= (18, 1) {
            "ResCountryState"
        } else {
            "CountryState"
        }
    }
});

/// Returns the correct class name for Partner/ResPartner depending on Odoo version
pub static PARTNER_CLASS_NAME: LazyLock<fn(OdooVersion) -> &'static str> = LazyLock::new(|| {
    |version: OdooVersion| {
        if version >= (18, 1) {
            "ResPartner"
        } else {
            "Partner"
        }
    }
});

/// Returns the correct class name for Country/ResCountry depending on Odoo version
pub static COUNTRY_CLASS_NAME: LazyLock<fn(OdooVersion) -> &'static str> = LazyLock::new(|| {
    |version: OdooVersion| {
        if version >= (18, 1) {
            "ResCountry"
        } else {
            "Country"
        }
    }
});

/// Resolves a list of evaluations to their final types, following references and
/// executing any `get_symbol_hook` attached along the way. Unlike
/// `get_resolved_symbols_at_position` (which inspects the raw evaluation pointer
/// without running hooks), this is needed to correctly resolve evaluations that
/// stay lazy/hook-based (e.g. class-member templates), matching what the hover
/// feature itself would show.
pub fn resolve_evaluation_types(session: &mut SessionInfo, evals: &[Evaluation]) -> Vec<SymbolKey> {
    evals
        .iter()
        .flat_map(|eval| {
            SymbolTable::follow_ref(
                &eval.symbol.get_symbol(session, None, &mut vec![], None),
                session,
                None,
                false,
                false,
                None,
                None,
            )
        })
        .collect::<Vec<_>>()
        .iter()
        .filter_map(|ev| ev.upgrade_weak(session.st()))
        .collect()
}

/// Resolves the evaluations stored on `symbol` (a variable or function) to their
/// final types. See `resolve_evaluation_types`.
pub fn resolve_symbol_types(session: &mut SessionInfo, symbol: SymbolKey) -> Vec<SymbolKey> {
    let evals = session.st().evaluations(symbol).cloned().unwrap_or_default();
    resolve_evaluation_types(session, &evals)
}

/// Get resolved symbols at a specific position in a source file. This follows references
/// and returns the final types of the evaluations found at the given line and character.
///
/// File must be pointing to a python file, otherwise it panics.
pub fn get_resolved_symbols_at_position(
    session: &mut SessionInfo,
    file_symbol: SourceFileKey,
    file_info: FileInfoKey,
    line: u32,
    character: u32,
) -> Vec<SymbolKey> {
    // Get evaluations at the given position
    let offset = session.file_mgr()[file_info]
        .position_to_offset(line, character, session.sync_odoo.encoding);
    let py_ast = session.file_mgr()[file_info].file_info_ast.ast.as_py_ast().cloned().expect("Expected Python AST");
    let (analyse_ast_result, _, _, _) =
        AstUtils::get_symbols(session, &py_ast, file_symbol, offset as u32);
    let evals = analyse_ast_result.evaluations;
    assert!(
        !evals.is_empty(),
        "No evaluations at line {line} character {character}"
    );

    // Follow refs for each evaluation to resolve final types
    evals
        .iter()
        .flat_map(|eval| {
            SymbolTable::follow_ref(
                eval.symbol.get_symbol_ptr(),
                session,
                None,
                false,
                false,
                None,
                None,
            )
        })
        .collect::<Vec<_>>()
        .iter().filter_map(|ev| ev.upgrade_weak(session.st()))
        .collect()
}

/// Validate `file_sym` again, e.g. after changing the Odoo version of the session.
pub fn revalidate(session: &mut SessionInfo, file_sym: SourceFileKey) {
    SymbolTable::invalidate(session, file_sym, BuildSteps::VALIDATION);
    BuildScheduler::process_rebuilds(session, false);
}
