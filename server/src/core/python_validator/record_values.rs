use crate::core::diagnostics::{create_diag, Diag, DiagnosticCode};
use crate::core::python_odoo_builder::MAGIC_FIELDS;
use crate::core::symbols::SymbolTable;
use crate::core::symbols::symbol_keys::{ModelSymbolKey, ModuleKey};
use crate::features::features_utils::SpecialArg;
use crate::threads::SessionInfo;
use ruff_python_ast::Expr;
use ruff_text_size::Ranged;

/// Validate the field *key/name* given to `create`, `write` or `update` called on `model`.
/// Each string key of a dict of values must be a field of the model, if the model is known.
pub(crate) fn validate_record_values_arg(
    session: &mut SessionInfo<'_>,
    special_arg: SpecialArg,
    model: Option<ModelSymbolKey>,
    from_module: Option<ModuleKey>,
    value: &Expr,
) -> Vec<Diag> {
    let Some(model) = model else {
        return vec![];
    };
    let mut diagnostics = vec![];
    for dict in special_arg.values_dicts(value) {
        for key in dict.iter_keys().flatten() {
            let Expr::StringLiteral(field_name) = key else {
                continue;
            };
            // A model defined in XML only has no class to get them from, but the registry gives the magic
            // fields to every model (not declared in the body of BaseModel up to 18.0)
            if matches!(model, ModelSymbolKey::XmlRecord(_)) && MAGIC_FIELDS.contains(&field_name.value.to_str()) {
                continue;
            }

            let is_empty = SymbolTable::get_member_symbol(
                session,
                model.into(),
                field_name.value.to_str(),
                from_module,
                false,
                true,
                false,
                false,
                false,
            )
            .0
            .is_empty();
            if is_empty
                && let Some(diagnostic) =
                    create_diag(session, DiagnosticCode::OLS03014, &[], field_name.range())
            {
                diagnostics.push(diagnostic);
            }
        }
    }
    diagnostics
}
