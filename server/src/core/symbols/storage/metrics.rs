use tracing::debug;
use crate::core::symbols::SymbolTable;

macro_rules! log_symbol_table {
    ($st:ident, $method:ident; $($field:ident),* $(,)?) => {{
        $(debug!("{}: {}", stringify!($field), $st.$field.$method());)*
    }};
}

pub fn log_symbol_counts(symbol_table: &SymbolTable) {
    debug!("=== Symbol counts ===");
    log_symbol_table!(symbol_table, len; roots, disk_dirs, namespaces, python_packages, modules, files, compiled, classes, functions, variables, xml_files, csv_files);
}

pub fn log_slotmap_capacities(symbol_table: &SymbolTable) {
    debug!("=== Slotmap capacities ===");
    log_symbol_table!(symbol_table, capacity; roots, disk_dirs, namespaces, python_packages, modules, files, compiled, classes, functions, variables, xml_files, csv_files);
}

pub fn log_memory_usage() {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        debug!("===Memory usage===");
        for line in status.lines() {
            if line.starts_with("VmRSS:") || line.starts_with("VmPeak:") || line.starts_with("VmHWM:") {
                debug!("{}", line);
            }
        }
    }
}

pub fn log_is_in_workspace(symbol_table: &SymbolTable) {
    debug!("=== Symbols in workspace ===");
    let total_files = symbol_table.files.len();
    let in_workspace_files = symbol_table.files.values().filter(|f| f.in_workspace).count();
    debug!("Files in workspace: {}/{}", in_workspace_files, total_files);

    let total_packages = symbol_table.python_packages.len();
    let in_workspace_packages = symbol_table.python_packages.values().filter(|p| p.in_workspace).count();
    debug!("Python packages in workspace: {}/{}", in_workspace_packages, total_packages);

    let total_modules = symbol_table.modules.len();
    let in_workspace_modules = symbol_table.modules.values().filter(|m| m.in_workspace).count();
    debug!("Modules in workspace: {}/{}", in_workspace_modules, total_modules);

    let total_xml_files = symbol_table.xml_files.len();
    let in_workspace_xml_files = symbol_table.xml_files.values().filter(|f| f.in_workspace).count();
    debug!("XML files in workspace: {}/{}", in_workspace_xml_files, total_xml_files);

    let total_csv_files = symbol_table.csv_files.len();
    let in_workspace_csv_files = symbol_table.csv_files.values().filter(|f| f.in_workspace).count();
    debug!("CSV files in workspace: {}/{}", in_workspace_csv_files, total_csv_files);
}

/// Memory profiling: per-slotmap len, capacity, element size.
pub fn symbol_table_report(st: &SymbolTable) -> Vec<(&'static str, usize, usize, usize)> {
    fn sz<K: slotmap::Key, V>(_m: &slotmap::SlotMap<K, V>) -> usize { std::mem::size_of::<V>() }
    macro_rules! rows { ($($f:ident),*) => { vec![$((stringify!($f), st.$f.len(), st.$f.capacity(), sz(&st.$f)),)*] } }
    rows!(roots, disk_dirs, namespaces, python_packages, modules, files, compiled, classes, functions, variables,
          xml_files, csv_files, xml_records, xml_fields, xml_menuitems, xml_templates, xml_assets, xml_deletes, js_files)
}

/// Memory profiling: drop slotmaps one by one, calling `mark` after each (destructive).
pub fn drop_slotmaps_one_by_one(st: &mut SymbolTable, mark: &dyn Fn(&str)) {
    macro_rules! d { ($($f:ident),*) => { $( { let m = std::mem::take(&mut st.$f); drop(m); mark(concat!("dropped_st:", stringify!($f))); } )* } }
    d!(xml_fields, xml_records, xml_templates, xml_menuitems, xml_assets, xml_deletes, xml_files, csv_files, js_files,
       variables, functions, classes, files, compiled, modules, python_packages, namespaces, disk_dirs, roots);
    let e = std::mem::take(&mut st.xml_declared_models); drop(e); mark("dropped_st:xml_declared_models");
}

/// Memory profiling: approximate heap held by the largest symbol fields.
pub fn symbol_field_report(st: &SymbolTable) -> Vec<String> {
    use std::mem::size_of;
    use crate::core::evaluation::{Evaluation, EvaluationValue};
    use crate::constants::OYarn;
    fn sym_maps_bytes(m: &crate::utils::HashMap<OYarn, crate::core::symbols::storage::symbol_mgr::SectionSymbols>) -> (usize, usize, usize) {
        let mut b = m.capacity() * (size_of::<OYarn>() + size_of::<crate::core::symbols::storage::symbol_mgr::SectionSymbols>() + 1);
        let mut inner = 0; let mut keys = 0;
        for v in m.values() { inner += 1; b += v.heap_bytes(); keys += v.len_keys(); }
        (b, inner, keys)
    }
    fn evals_bytes(e: &[Evaluation], cap: usize) -> (usize, usize) {
        let mut b = cap * size_of::<Evaluation>(); let mut with_value = 0;
        for ev in e { if let Some(v) = &ev.value { with_value += 1; b += match v {
            EvaluationValue::CONSTANT(_) => size_of::<ruff_python_ast::Expr>(),
            EvaluationValue::DICT(d) => d.capacity() * 2 * size_of::<ruff_python_ast::Expr>(),
            EvaluationValue::LIST(l) | EvaluationValue::TUPLE(l) => l.capacity() * size_of::<ruff_python_ast::Expr>(),
            _ => 0 }; } }
        (b, with_value)
    }
    let mut out = vec![];
    // functions
    let (mut doc, mut diag_cap, mut diag_n, mut ev_b, mut ev_v, mut args_b, mut sec_b, mut sm_b, mut sm_inner, mut sm_keys) = (0,0,0,0,0,0,0,0,0,0);
    for f in st.functions.values() {
        doc += f.doc_string.as_ref().map(|d| d.capacity()).unwrap_or(0);
        diag_cap += f.diagnostics.capacity(); diag_n += f.diagnostics.values().map(|v| v.len()).sum::<usize>();
        let (b, v) = evals_bytes(&f.evaluations, f.evaluations.capacity()); ev_b += b; ev_v += v;
        args_b += f.args.capacity() * size_of::<crate::core::symbols::storage::function_symbol::Argument>();
        sec_b += f.sections.capacity() * size_of::<crate::core::symbols::storage::symbol_mgr::SectionRange>();
        let (b, i, k) = sym_maps_bytes(&f.symbols); sm_b += b; sm_inner += i; sm_keys += k;
    }
    out.push(format!("fn:doc={doc}:diag_map_cap={diag_cap}:diags={diag_n}:evals_b={ev_b}:evals_with_value={ev_v}:args_b={args_b}:sections_b={sec_b}:symmaps_b={sm_b}:inner_maps={sm_inner}:keys={sm_keys}"));
    let (mut doc, mut ev_b, mut ev_v, mut n_ev) = (0,0,0,0);
    for v in st.variables.values() {
        doc += v.doc_string.as_ref().map(|d| d.capacity()).unwrap_or(0);
        let (b, vv) = evals_bytes(&v.evaluations, v.evaluations.capacity()); ev_b += b; ev_v += vv; n_ev += v.evaluations.len();
    }
    out.push(format!("var:doc={doc}:evals={n_ev}:evals_b={ev_b}:evals_with_value={ev_v}:eval_size={}:expr_size={}", size_of::<Evaluation>(), size_of::<ruff_python_ast::Expr>()));
    let (mut doc, mut sm_b, mut sec_b) = (0,0,0);
    for c in st.classes.values() { doc += c.doc_string.as_ref().map(|d| d.capacity()).unwrap_or(0); sm_b += sym_maps_bytes(&c.symbols).0; sec_b += c.sections.capacity() * size_of::<crate::core::symbols::storage::symbol_mgr::SectionRange>(); }
    out.push(format!("class:doc={doc}:symmaps_b={sm_b}:sections_b={sec_b}"));
    let (mut sm_b, mut path_b) = (0,0);
    for f in st.files.values() { sm_b += sym_maps_bytes(&f.symbols).0; path_b += f.path.capacity(); }
    out.push(format!("file:symmaps_b={sm_b}:path_b={path_b}"));
    let (mut text_b, mut ref_b) = (0,0);
    for x in st.xml_fields.values() { text_b += x.text.as_ref().map(|t| t.capacity()).unwrap_or(0); ref_b += x.ref_key.as_ref().map(|r| r.0.capacity()).unwrap_or(0); }
    out.push(format!("xml_field:text_b={text_b}:ref_b={ref_b}"));
    out
}
