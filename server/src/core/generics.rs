//! Generic types.
//!
//! - A parameterized type keeps its type arguments in the context of its evaluation
//!   (`ContextKey::TypeArgs`): `list[Item]` is the class `list` with `[[Item]]`.
//! - A generic class records its type parameters and the type arguments it gives to its
//!   bases (`ClassGenerics`, filled when its bases are loaded).
//! - A member found on a generic class gets the values of that class's type variables
//!   (`ContextKey::TypeBindings`), computed along the bases from the class of the receiver;
//!   the results of the member (return of a method, item of `__getitem__`...) are then
//!   substituted with them.
//! - The special forms of `typing` (`Optional`, `Union`, `List`, `Type`...) are recognized
//!   by name in `typing`/`typing_extensions`.

use crate::constants::SymType;
use crate::core::evaluation::{EvaluationSymbolPtr, EvaluationSymbolWeak};
use crate::core::evaluation_context::{Context, ContextKey, ContextValue};
use crate::core::symbols::function_symbol::ArgumentType;
use crate::core::symbols::symbol_keys::{FunctionKey, SymbolKey, Wk};
use crate::threads::SessionInfo;
use crate::utils::HashSet;

pub type TypeArgs = Vec<Vec<EvaluationSymbolPtr>>;
pub type Bindings = Vec<(Wk<SymbolKey>, Vec<EvaluationSymbolPtr>)>;

/// Type parameters of a generic class and the type arguments it gives to its bases.
#[derive(Debug, Clone, Default)]
pub struct ClassGenerics {
    /// Its type variables, in order.
    pub params: Vec<Wk<SymbolKey>>,
    /// `class C(Base[X, T])`: (Base, [[X], [T]]).
    pub base_args: Vec<(Wk<SymbolKey>, TypeArgs)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpecialForm {
    /// `Optional[X]`: X or None.
    Optional,
    /// `Union[A, B]`: A or B.
    Union,
    /// `Annotated[X, ...]`, `ClassVar[X]`, `Final[X]`...: X.
    Unwrap,
    /// `type[X]`, `Type[X]`: the class X itself.
    Type,
    /// `List[X]`, `Dict[K, V]`...: the class of `builtins` or `collections`.
    Alias(&'static str, &'static str),
    /// `Generic[T, ...]`: declares type parameters.
    Generic,
    /// `Protocol[T, ...]`: declares type parameters of a protocol.
    Protocol,
    /// `Literal[...]`, `Callable[...]`...: not modeled, evaluates to nothing as before.
    Opaque,
}

const TYPING_MODULES: [&str; 2] = ["typing", "typing_extensions"];

/// The special form `sym` is, if any. `sym` must have been resolved through imports.
pub fn special_form(session: &SessionInfo, sym: SymbolKey) -> Option<SpecialForm> {
    let name = session.st().name(sym).to_string();
    if name == "type" {
        return session.sync_odoo.match_tree_from_any_entry(sym, (&["builtins"], &["type"])).then_some(SpecialForm::Type);
    }
    let form = match name.as_str() {
        "Optional" => SpecialForm::Optional,
        "Union" => SpecialForm::Union,
        "Annotated" | "ClassVar" | "Final" | "Required" | "NotRequired" | "ReadOnly" => SpecialForm::Unwrap,
        "Type" => SpecialForm::Type,
        "List" => SpecialForm::Alias("builtins", "list"),
        "Dict" => SpecialForm::Alias("builtins", "dict"),
        "Set" => SpecialForm::Alias("builtins", "set"),
        "FrozenSet" => SpecialForm::Alias("builtins", "frozenset"),
        "Tuple" => SpecialForm::Alias("builtins", "tuple"),
        "DefaultDict" => SpecialForm::Alias("collections", "defaultdict"),
        "OrderedDict" => SpecialForm::Alias("collections", "OrderedDict"),
        "Counter" => SpecialForm::Alias("collections", "Counter"),
        "Deque" => SpecialForm::Alias("collections", "deque"),
        "ChainMap" => SpecialForm::Alias("collections", "ChainMap"),
        "Generic" => SpecialForm::Generic,
        "Protocol" => SpecialForm::Protocol,
        "Literal" | "Callable" | "Concatenate" | "TypeGuard" | "TypeIs" | "Unpack" => SpecialForm::Opaque,
        _ => return None,
    };
    TYPING_MODULES
        .iter()
        .any(|module| session.sync_odoo.match_tree_from_any_entry(sym, (&[module], &[name.as_str()])))
        .then_some(form)
}

/// Follows import variables (`from typing import Optional`) to the symbol they import.
pub fn resolve_imports(session: &SessionInfo, sym: SymbolKey) -> SymbolKey {
    let mut current = sym;
    for _ in 0..16 {
        let SymbolKey::Variable(v) = current else { break };
        let var = &session.st()[v];
        if !var.is_import_variable || var.evaluations.len() != 1 {
            break;
        }
        match var.evaluations[0].symbol.get_symbol_ptr().upgrade_weak(session.st()) {
            Some(next) if next != current => current = next,
            _ => break,
        }
    }
    current
}

/// Whether `sym` is a type variable: `T = TypeVar("T")`, or a PEP 695 type parameter.
pub fn is_type_var(session: &SessionInfo, sym: SymbolKey) -> bool {
    let SymbolKey::Variable(v) = resolve_imports(session, sym) else { return false };
    let var = &session.st()[v];
    if var.is_type_param {
        return true;
    }
    var.evaluations.iter().any(|eval| {
        let ptr = eval.symbol.get_symbol_ptr();
        ptr.has_weak()
            && ptr.get_weak().instance == Some(true)
            && ptr.upgrade_weak(session.st()).is_some_and(|class| {
                matches!(class, SymbolKey::Class(_))
                    && session.st().name(class) == "TypeVar"
                    && TYPING_MODULES
                        .iter()
                        .any(|module| session.sync_odoo.match_tree_from_any_entry(class, (&[module], &["TypeVar"])))
            })
    })
}

/// The type variable `ptr` points to, normalized through imports.
pub fn type_var_of(session: &SessionInfo, ptr: &EvaluationSymbolPtr) -> Option<Wk<SymbolKey>> {
    let sym = ptr.upgrade_weak(session.st())?;
    is_type_var(session, sym).then(|| resolve_imports(session, sym).into())
}

pub fn type_args(ptr: &EvaluationSymbolPtr) -> Option<&TypeArgs> {
    if !ptr.has_weak() {
        return None;
    }
    match ptr.get_weak().context.get(ContextKey::TypeArgs) {
        Some(ContextValue::TypeArgs(args)) => Some(args),
        _ => None,
    }
}

pub fn bindings(context: &Context) -> Option<&Bindings> {
    match context.get(ContextKey::TypeBindings) {
        Some(ContextValue::TypeBindings(bindings)) => Some(bindings),
        _ => None,
    }
}

/// `ptr` with these type arguments.
pub fn with_type_args(mut ptr: EvaluationSymbolPtr, args: TypeArgs) -> EvaluationSymbolPtr {
    if ptr.has_weak() && !args.is_empty() {
        ptr.get_mut_weak().context.insert(ContextKey::TypeArgs, ContextValue::TypeArgs(args));
    }
    ptr
}

fn generics_of<'a>(session: &'a SessionInfo, class: SymbolKey) -> Option<&'a ClassGenerics> {
    let SymbolKey::Class(c) = class else { return None };
    session.st()[c].generics.as_deref()
}

/// Whether a member found on `owner` of an object of class `receiver` may depend on type
/// variables: one of them is generic, or the object has type arguments.
pub fn may_bind(session: &SessionInfo, receiver: SymbolKey, owner: SymbolKey, has_args: bool) -> bool {
    has_args || generics_of(session, receiver).is_some() || generics_of(session, owner).is_some()
}

/// The values of the type variables of `owner`, for an object of class `receiver` with
/// type arguments `args`. Walks the bases from `receiver`, substituting the type
/// arguments each class gives to its bases; each class gets its own bindings, as stubs
/// reuse the same type variable in unrelated classes.
pub fn bindings_for(session: &SessionInfo, receiver: SymbolKey, args: Option<&TypeArgs>, owner: SymbolKey) -> Bindings {
    let params_of = |class: SymbolKey| generics_of(session, class).map(|g| g.params.clone()).unwrap_or_default();
    let zip = |params: Vec<Wk<SymbolKey>>, args: Vec<Vec<EvaluationSymbolPtr>>| -> Bindings {
        params.into_iter().zip(args).filter(|(_, types)| !types.is_empty()).collect()
    };
    let first = zip(params_of(receiver), args.cloned().unwrap_or_default());
    let mut queue = std::collections::VecDeque::from([(receiver, first)]);
    let mut visited = HashSet::default();
    while let Some((class, env)) = queue.pop_front() {
        if class == owner {
            return env;
        }
        if !visited.insert(class) {
            continue;
        }
        let SymbolKey::Class(c) = class else { continue };
        let mut with_args = HashSet::default();
        if let Some(generics) = generics_of(session, class) {
            for (base, base_args) in generics.base_args.iter() {
                let Some(base) = base.upgrade(session.st()) else { continue };
                with_args.insert(base);
                let substituted = base_args.iter().map(|alts| substitute_all(session, alts, &env)).collect();
                queue.push_back((base, zip(params_of(base), substituted)));
            }
        }
        for base in session.st()[c].bases.iter().filter_map(|b| b.upgrade(session.st())) {
            let base: SymbolKey = base.into();
            if !with_args.contains(&base) {
                queue.push_back((base, vec![]));
            }
        }
    }
    vec![]
}

pub fn substitute_all(session: &SessionInfo, ptrs: &[EvaluationSymbolPtr], bindings: &Bindings) -> Vec<EvaluationSymbolPtr> {
    ptrs.iter().flat_map(|ptr| substitute(session, ptr, bindings)).collect()
}

/// `ptr` with its type variables replaced by their values: a type variable becomes its
/// possible types, type arguments are substituted recursively.
pub fn substitute(session: &SessionInfo, ptr: &EvaluationSymbolPtr, bindings: &Bindings) -> Vec<EvaluationSymbolPtr> {
    if bindings.is_empty() {
        return vec![ptr.clone()];
    }
    if let Some(var) = type_var_of(session, ptr) {
        return match bindings.iter().find(|(v, _)| *v == var) {
            // `-> type[T]`: the class itself.
            Some((_, types)) if ptr.get_weak().instance == Some(false) => types.iter().cloned().map(|mut t| {
                if t.has_weak() {
                    t.get_mut_weak().instance = Some(false);
                }
                t
            }).collect(),
            Some((_, types)) => types.clone(),
            None => vec![ptr.clone()],
        };
    }
    match type_args(ptr) {
        Some(args) => {
            let args = args.iter().map(|alts| substitute_all(session, alts, bindings)).collect();
            vec![with_type_args(ptr.clone(), args)]
        }
        None => vec![ptr.clone()],
    }
}

/// The class `member` is defined in.
pub fn owner_class(session: &SessionInfo, member: SymbolKey) -> Option<SymbolKey> {
    session.st().get_in_parents(member, &[SymType::CLASS], true)
}

/// `f` and the `@overload` definitions before it, last first.
pub fn overload_chain(session: &SessionInfo, f: FunctionKey) -> Vec<FunctionKey> {
    let mut chain = vec![f];
    let mut current = f;
    loop {
        let func = &session.st()[current];
        let previous = session.st().get_content_symbol(func.parent().into(), &func.name, func.range.start().to_u32()).symbols;
        match previous.last() {
            Some(&SymbolKey::Function(p)) if p != current && session.st()[p].is_overloaded && !chain.contains(&p) => {
                chain.push(p);
                current = p;
            }
            _ => break,
        }
    }
    chain
}

/// The overloads of `f` that accept `positional` positional and `keywords` keyword arguments
/// (`skip_first`: the first parameter is bound, `self` or `cls`). All of them when none does.
pub fn select_call_overloads(session: &SessionInfo, f: FunctionKey, call: &ruff_python_ast::Arguments, skip_first: bool) -> Vec<FunctionKey> {
    let (positional, keywords) = (call.args.len(), call.keywords.len());
    let chain = overload_chain(session, f);
    if chain.len() < 2 {
        return chain;
    }
    let accepts = |candidate: &FunctionKey| {
        let func = &session.st()[*candidate];
        let args = func.args.iter().skip(usize::from(skip_first));
        let (mut required, mut max, mut unbounded) = (0usize, 0usize, false);
        for arg in args {
            match arg.arg_type {
                ArgumentType::VARARG | ArgumentType::KWARG => unbounded = true,
                ArgumentType::KWORD_ONLY => {
                    if arg.default_value.is_none() {
                        required += 1;
                    }
                    max += 1;
                }
                ArgumentType::POS_ONLY | ArgumentType::ARG => {
                    if arg.default_value.is_none() {
                        required += 1;
                    }
                    max += 1;
                }
            }
        }
        let given = positional + keywords;
        // `def get(self, key, default: None = None)`: only when that argument is None.
        let none_mismatch = func.args.iter().skip(usize::from(skip_first))
            .zip(call.args.iter())
            .any(|(arg, value)| matches!(arg.annotation.as_deref(), Some(ruff_python_ast::Expr::NoneLiteral(_)))
                && !matches!(value, ruff_python_ast::Expr::NoneLiteral(_)));
        given >= required && (unbounded || given <= max) && !none_mismatch
    };
    // Stubs list the overloads from the most specific: keep their order.
    let selected: Vec<FunctionKey> = chain.iter().rev().copied().filter(accepts).collect();
    if selected.is_empty() { chain } else { selected }
}

/// A weak evaluation of `sym`.
pub fn weak(sym: impl Into<Wk<SymbolKey>>, instance: Option<bool>) -> EvaluationSymbolPtr {
    EvaluationSymbolPtr::WEAK(EvaluationSymbolWeak::new(sym, instance, false))
}

/// The alternatives of a union that are modeled: `X | None` is X or None, as before a union
/// of several types is left unknown (calls and attributes would be checked against each
/// of them, without the narrowing `isinstance`/`match` give at run time).
/// Type variables are kept: they are substituted later (`def get(...) -> _VT | _T`).
pub fn optional_union(session: &SessionInfo, alternatives: Vec<EvaluationSymbolPtr>) -> Vec<EvaluationSymbolPtr> {
    let mut types: Vec<EvaluationSymbolPtr> = vec![];
    for alternative in alternatives.iter().filter(|a| !matches!(a, EvaluationSymbolPtr::NONE) && type_var_of(session, a).is_none()) {
        if !types.contains(alternative) {
            types.push(alternative.clone());
        }
    }
    if types.len() > 1 {
        return vec![];
    }
    alternatives
}

/// Whether `ptr` is or contains a type variable.
pub fn has_type_vars(session: &SessionInfo, ptr: &EvaluationSymbolPtr) -> bool {
    type_var_of(session, ptr).is_some()
        || type_args(ptr).is_some_and(|args| args.iter().flatten().any(|arg| has_type_vars(session, arg)))
}

fn push_binding(bindings: &mut Bindings, var: Wk<SymbolKey>, value: EvaluationSymbolPtr) {
    match bindings.iter_mut().find(|(v, _)| *v == var) {
        Some((_, values)) => {
            if !values.contains(&value) {
                values.push(value);
            }
        }
        None => bindings.push((var, vec![value])),
    }
}

/// Members whose return type can tie a protocol's type variable to a class structurally
/// (`SupportsNext[T].__next__ -> T`).
const STRUCTURAL_MEMBERS: [&str; 5] = ["__next__", "__iter__", "__getitem__", "__enter__", "__call__"];

/// The values of the type variables of `protocol` for an object of class `class` that does
/// not inherit it: for each member of `protocol` returning one of its type variables, what
/// the same member of `class` returns.
fn structural_bindings(session: &mut SessionInfo, class: SymbolKey, args: Option<&TypeArgs>, protocol: SymbolKey) -> Bindings {
    let mut out = vec![];
    let params = generics_of(session, protocol).map(|g| g.params.clone()).unwrap_or_default();
    if params.is_empty() {
        return out;
    }
    // The class must have every method of the protocol (`list` is not a
    // `SupportsKeysAndGetItem`: it has `__getitem__` but no `keys`).
    let methods: Vec<String> = session.st().iter_symbols(protocol)
        .filter(|(_, section)| section.values().flat_map(|keys| keys.iter()).any(|s| matches!(s, SymbolKey::Function(_))))
        .map(|(name, _)| name.to_string())
        .filter(|name| !matches!(name.as_str(), "__init__" | "__new__" | "__slots__" | "__class_getitem__" | "__subclasshook__"))
        .collect();
    for method in methods.iter() {
        if crate::core::symbols::storage::SymbolTable::get_member_symbol(session, class, method, None, true, false, false, false, false).0.is_empty() {
            return out;
        }
    }
    for name in STRUCTURAL_MEMBERS {
        let (protocol_members, _) = crate::core::symbols::storage::SymbolTable::get_member_symbol(session, protocol, name, None, true, false, false, false, false);
        let Some(&SymbolKey::Function(protocol_member)) = protocol_members.first() else { continue };
        crate::core::odoo::SyncOdoo::ensure_generic_func_evaluations(session, protocol_member);
        let returned: Vec<Wk<SymbolKey>> = session.st()[protocol_member].evaluations.iter()
            .filter_map(|eval| type_var_of(session, eval.symbol.get_symbol_ptr()))
            .filter(|var| params.contains(var))
            .collect();
        if returned.is_empty() {
            continue;
        }
        let (class_members, _) = crate::core::symbols::storage::SymbolTable::get_member_symbol(session, class, name, None, true, false, false, false, false);
        let Some(&SymbolKey::Function(class_member)) = class_members.first() else { continue };
        let member_bindings = owner_class(session, class_member.into())
            .map(|owner| bindings_for(session, class, args, owner))
            .unwrap_or_default();
        crate::core::odoo::SyncOdoo::ensure_generic_func_evaluations(session, class_member);
        let values: Vec<EvaluationSymbolPtr> = session.st()[class_member].evaluations.clone().iter()
            .flat_map(|eval| substitute(session, eval.symbol.get_symbol_ptr(), &member_bindings))
            .filter(|value| type_var_of(session, value).is_none())
            .collect();
        for var in returned {
            for value in values.iter() {
                push_binding(&mut out, var, value.clone());
            }
        }
    }
    out
}

/// Binds the type variables of `param` (the declared type of a parameter) from `arg` (the
/// type of the argument given to it): `T` <- arg; `Iterable[T]` <- `list[Item]` through the
/// bases of `list`, or structurally for a protocol.
pub fn bind_param(session: &mut SessionInfo, param: &EvaluationSymbolPtr, arg: &EvaluationSymbolPtr, out: &mut Bindings, depth: u32) {
    if depth > 8 {
        return;
    }
    if let Some(var) = type_var_of(session, param) {
        let mut value = arg.clone();
        // `type_: type[T]` given the class X: T is an instance of X.
        if param.get_weak().instance == Some(false) {
            if !(value.has_weak() && value.get_weak().instance == Some(false)) {
                return;
            }
            value.get_mut_weak().instance = Some(true);
        }
        if value.has_weak() || matches!(value, EvaluationSymbolPtr::NONE) {
            push_binding(out, var, value);
        }
        return;
    }
    let Some(param_args) = type_args(param).cloned() else { return };
    let (Some(param_class), Some(arg_class)) = (param.upgrade_weak(session.st()), arg.upgrade_weak(session.st())) else { return };
    if !matches!(arg_class, SymbolKey::Class(_)) {
        return;
    }
    let arg_args = type_args(arg).cloned();
    let mut env = bindings_for(session, arg_class, arg_args.as_ref(), param_class);
    if env.is_empty() {
        env = structural_bindings(session, arg_class, arg_args.as_ref(), param_class);
    }
    let params = generics_of(session, param_class).map(|g| g.params.clone()).unwrap_or_default();
    for (index, var) in params.iter().enumerate() {
        let (Some((_, values)), Some(alternatives)) = (env.iter().find(|(v, _)| v == var).cloned(), param_args.get(index)) else { continue };
        for alternative in alternatives {
            for value in values.iter() {
                bind_param(session, alternative, value, out, depth + 1);
            }
        }
    }
}

/// The type variables of function `f` bound from the types of the positional arguments of a
/// call (`skip_first`: the first parameter is bound, `self` or `cls`).
pub fn infer_call_bindings(session: &mut SessionInfo, f: FunctionKey, positional: &[Vec<EvaluationSymbolPtr>], skip_first: bool) -> Bindings {
    let mut out = vec![];
    let func = &session.st()[f];
    let skip = usize::from(skip_first);
    let args: Vec<(Option<crate::core::symbols::symbol_keys::VariableKey>, Option<Box<ruff_python_ast::Expr>>)> = func.args.iter()
        .skip(skip)
        .filter(|arg| matches!(arg.arg_type, ArgumentType::POS_ONLY | ArgumentType::ARG))
        .map(|arg| (arg.symbol.upgrade(session.st()), arg.annotation.clone()))
        .collect();
    let scope: SymbolKey = func.parent().into();
    let position = func.range.start();
    let mut params: Vec<Vec<EvaluationSymbolPtr>> = vec![];
    for (variable, annotation) in args.into_iter().take(positional.len()) {
        let Some(variable) = variable else {
            params.push(vec![]);
            continue;
        };
        // A stub evaluated before the modules it imports from (`builtins` and `collections.abc`)
        // may have left an annotation unevaluated: evaluate it now, once.
        if session.st()[variable].evaluations.is_empty() && let Some(annotation) = annotation {
            let (evals, _) = crate::core::evaluation::Evaluation::eval_from_ast(session, &annotation, scope, &position, true, &mut vec![vec![], vec![]]);
            session.st_mut()[variable].evaluations = evals;
        }
        params.push(session.st()[variable].evaluations.iter().map(|eval| eval.symbol.get_symbol_ptr().clone()).collect());
    }
    for (param_types, arg_types) in params.iter().zip(positional) {
        for param in param_types {
            for arg in arg_types {
                bind_param(session, param, arg, &mut out, 0);
            }
        }
    }
    out
}

/// `list[Item]`, `dict[str, Item | None]`: the type arguments of `context` after `name`.
pub fn display_with_type_args(session: &SessionInfo, name: &str, context: &Context) -> String {
    let Some(ContextValue::TypeArgs(args)) = context.get(ContextKey::TypeArgs) else {
        return name.to_string();
    };
    format!("{name}[{}]", args.iter().map(|alternatives| display_alternatives(session, alternatives, 0)).collect::<Vec<_>>().join(", "))
}

fn display_alternatives(session: &SessionInfo, alternatives: &[EvaluationSymbolPtr], depth: u32) -> String {
    if alternatives.is_empty() {
        return "Any".to_string();
    }
    let mut names: Vec<String> = alternatives.iter().map(|ptr| display_ptr(session, ptr, depth)).collect();
    names.dedup();
    names.join(" | ")
}

fn display_ptr(session: &SessionInfo, ptr: &EvaluationSymbolPtr, depth: u32) -> String {
    match ptr {
        EvaluationSymbolPtr::NONE => "None".to_string(),
        EvaluationSymbolPtr::WEAK(w) | EvaluationSymbolPtr::SELF(w) => {
            let Some(sym) = w.weak.upgrade(session.st()) else { return "Any".to_string() };
            let name = session.st().name(sym).to_string();
            let name = if name == "ellipsis" { "...".to_string() } else { name };
            match type_args(ptr) {
                Some(args) if depth < 4 => format!("{name}[{}]", args.iter().map(|a| display_alternatives(session, a, depth + 1)).collect::<Vec<_>>().join(", ")),
                _ => name,
            }
        }
        _ => "Any".to_string(),
    }
}
