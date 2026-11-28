//! Generic types: `typing` special forms, parameterized containers, generic classes and
//! functions (PEP 484 and PEP 695), iteration and indexing. Fixture: `generics.py`.

use crate::common;

use odoo_ls_server::core::file_mgr::FileInfoKey;
use odoo_ls_server::core::odoo::SyncOdoo;
use odoo_ls_server::core::symbols::symbol_keys::SourceFileKey;
use odoo_ls_server::threads::SessionInfo;
use odoo_ls_server::utils::PathSanitizer;
use std::env;
use std::path::Path;

fn fixture_path() -> String {
    env::current_dir().unwrap().join("tests/data/python/expressions/generics.py").sanitize()
}

struct Fixture {
    file_info: FileInfoKey,
    file_symbol: SourceFileKey,
    content: String,
}

fn load(session: &mut SessionInfo) -> Fixture {
    let path = fixture_path();
    common::prepare_custom_entry_point(session, path.as_str());
    let file_info = session.file_mgr().get_file_info(&path).unwrap();
    let file_symbol = SyncOdoo::get_symbol_of_opened_file(session, Path::new(&path)).unwrap();
    Fixture { file_info, file_symbol, content: std::fs::read_to_string(&path).unwrap() }
}

/// Position of the last character of the only line of the fixture that is exactly `probe`
/// (indentation aside).
fn probe_position(fixture: &Fixture, probe: &str) -> (u32, u32) {
    let lines: Vec<(usize, &str)> = fixture.content.lines().enumerate().filter(|(_, l)| l.trim() == probe).collect();
    assert_eq!(lines.len(), 1, "probe `{probe}` must be on exactly one line of generics.py");
    let (line, text) = lines[0];
    (line as u32, (text.len() - 1) as u32)
}

fn resolved_names(session: &mut SessionInfo, fixture: &Fixture, probe: &str) -> Vec<String> {
    let (line, character) = probe_position(fixture, probe);
    let mut names: Vec<String> = common::get_resolved_symbols_at_position(session, fixture.file_symbol, fixture.file_info, line, character)
        .into_iter()
        .map(|s| session.st().name(s).to_string())
        .collect();
    names.sort();
    names.dedup();
    names
}

fn hover(session: &mut SessionInfo, fixture: &Fixture, probe: &str) -> String {
    let (line, character) = probe_position(fixture, probe);
    common::get_hover_markdown(session, fixture.file_symbol, fixture.file_info, line, character).unwrap_or_default()
}

/// Each probe resolves to `Item`: the type argument has been carried and substituted.
pub fn test_generic_types_resolve(mut session: SessionInfo) {
    let fixture = load(&mut session);
    let cases: &[(&str, &str)] = &[
        // parameters annotated with parameterized types
        ("xs[0]", "list[Item] parameter, indexed"),
        ("m[\"a\"]", "dict[str, Item] parameter, indexed"),
        ("box.get()", "Box[Item] parameter, method of a generic class"),
        // containers returned by functions
        ("a", "list[Item]()[0]"),
        ("b", "List[Item]()[0] (typing alias)"),
        ("d", "dict[str, Item]()[key]"),
        ("g", "Dict[str, Item]()[key]"),
        ("n", "Sequence[Item]()[0] (inherited __getitem__)"),
        ("q", "list[list[Item]]()[0][0] (nested)"),
        ("z", "dict.get(key, default) overload"),
        // iteration
        ("c", "for over list[Item]"),
        ("f", "for over dict.values()"),
        ("o", "for over Iterator[Item]"),
        ("value", "for key, value in dict.items()"),
        ("item", "for name, item in dict.items()"),
        ("val", "for pos, val in enumerate(list[Item])"),
        // special forms
        ("i", "type[Item]"),
        ("j", "Type[Item]"),
        ("cls", "cls: type[Item]"),
        // generic classes
        ("k", "Box[Item]().get()"),
        ("l", "Registry[str, Item]().lookup() (dict, Generic[K, V])"),
        ("m", "ItemList(List[Item])()[0]"),
        ("r", "Box[Item]().items()[0]"),
        ("s", "ItemBox(Box[Item])().get() (inherited binding)"),
        ("t", "StrMap(Mapping[str, Item])()[key]"),
        ("annotated[0]", "annotated variable list[Item]"),
        ("built.get()", "Box(Item()): bound from the constructor"),
        ("copied", "list(list[Item])[0]: bound from the constructor"),
        // generic functions
        ("p", "next(Iterator[Item]) (structural SupportsNext)"),
        ("u", "first(xs: Iterable[T]) -> T"),
        ("w", "pair(K, V) -> Dict[K, V]"),
        ("x", "ident(x: T) -> T"),
        ("y", "sorted(list[Item])[0] (typeshed overload)"),
        ("f2", "collections.abc.Iterable[T] parameter"),
        // PEP 695
        ("s1", "class Stack[S], constructor binding"),
        ("s2", "class ItemStack(Stack[Item])"),
        ("s3", "def last[L](values: list[L]) -> L"),
        ("s4", "-> Stack[Item]"),
        ("parsed", "parse_as(type_: type[T]) -> T given the class"),
    ];
    let mut failures = vec![];
    for (probe, case) in cases {
        let names = resolved_names(&mut session, &fixture, probe);
        if names != ["Item"] {
            failures.push(format!("{case} (`{probe}`): {names:?}"));
        }
    }
    assert!(failures.is_empty(), "not resolved to Item:\n{}", failures.join("\n"));
}

/// `Optional[X]`, `X | None` and `dict.get(key)` give X or None.
pub fn test_optional_and_union(mut session: SessionInfo) {
    let fixture = load(&mut session);
    for probe in ["e", "h"] {
        let shown = hover(&mut session, &fixture, probe);
        assert!(shown.contains("Item | None"), "`{probe}` should be `Item | None`, hover: {shown}");
    }
}

/// `type[T]`: T is an instance of the class given, `-> type[T]` is the class itself.
pub fn test_type_of_type_variable(mut session: SessionInfo) {
    let fixture = load(&mut session);
    let parsed = hover(&mut session, &fixture, "parsed");
    assert!(parsed.contains("parsed: Item"), "instance expected: {parsed}");
    let klass = hover(&mut session, &fixture, "klass2");
    assert!(klass.contains("(class)") || klass.contains("type[Item]") || klass.contains("klass2: Item"), "class expected: {klass}");
    let (line, character) = probe_position(&fixture, "klass2");
    let offset = session.file_mgr()[fixture.file_info].position_to_offset(line, character, session.sync_odoo.encoding);
    let py_ast = session.file_mgr()[fixture.file_info].file_info_ast.ast.as_py_ast().cloned().unwrap();
    let (result, _, _, _) = odoo_ls_server::features::ast_utils::AstUtils::get_symbols(&mut session, &py_ast, fixture.file_symbol, offset as u32);
    let instances: Vec<Option<bool>> = result.evaluations.iter()
        .flat_map(|eval| odoo_ls_server::core::symbols::storage::SymbolTable::follow_ref(eval.symbol.get_symbol_ptr(), &mut session, None, false, false, None, None))
        .map(|ptr| match ptr { odoo_ls_server::core::evaluation::EvaluationSymbolPtr::WEAK(w) | odoo_ls_server::core::evaluation::EvaluationSymbolPtr::SELF(w) => w.instance, _ => None })
        .collect();
    assert_eq!(instances, vec![Some(false)], "klass_of(Item()) is the class Item");
}

/// Hover shows the type arguments.
pub fn test_hover_shows_type_arguments(mut session: SessionInfo) {
    let fixture = load(&mut session);
    for (probe, expected) in [("shown", "dict[str, Item]"), ("sr", "list[Item]"), ("e2", "tuple[int, Item]"), ("nx", "tuple[int, Item]")] {
        let shown = hover(&mut session, &fixture, probe);
        assert!(shown.contains(expected), "`{probe}` should show `{expected}`, hover: {shown}");
    }
}

/// Without type arguments, a type variable is not guessed.
pub fn test_unbound_type_variable_is_not_guessed(mut session: SessionInfo) {
    let fixture = load(&mut session);
    let shown = hover(&mut session, &fixture, "unbound");
    assert!(shown.contains("unbound: Any"), "Box without type arguments: {shown}");
}

/// `Generic[...]`, `Protocol[...]`, `List[...]`, `Mapping[...]` and PEP 695 bases are no
/// longer reported as unresolved (OLS01001 `//Unhandled// not found`).
pub fn test_generic_bases_have_no_diagnostics(mut session: SessionInfo) {
    let path = fixture_path();
    let _ = load(&mut session);
    let diagnostics = common::get_diagnostics_for_path(&mut session, &path);
    assert!(diagnostics.is_empty(), "unexpected diagnostics: {:?}", diagnostics.iter().map(|d| (d.range.start.line, &d.code, &d.message)).collect::<Vec<_>>());
}
