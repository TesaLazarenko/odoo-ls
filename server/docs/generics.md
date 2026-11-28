# Generic types

The server keeps the type arguments of parameterized types and substitutes the type
variables of generic classes and functions. Code: `server/src/core/generics.rs`, tests:
`server/tests/python/test_generics.rs` on `tests/data/python/expressions/generics.py`.

## What is supported

| Case | Example | Result |
|---|---|---|
| Containers | `def f() -> list[Item]`, `f()[0]`, `d: dict[str, Item]`, `d["k"]`, `d.get("k")` | `Item`, `Item \| None` |
| `for` loops | `for x in items`, `for k, v in d.items()`, `for i, x in enumerate(items)` | `Item`, `str`/`Item`, `int`/`Item` |
| Nested | `list[list[Item]]()[0][0]` | `Item` |
| Generic classes | `class Box(Generic[T])`, `Box[Item]().get()`, `class ItemBox(Box[Item])` | `Item` |
| Inherited bases | `class R(dict, Generic[K, V])`, `class S(Mapping[str, Item])` | members of `dict`, `Mapping` with their types |
| Generic functions | `def first(xs: Iterable[T]) -> T`, `sorted(items)`, `next(it)` | `Item` |
| Constructors | `Box(Item())`, `list(items)`, `dict(...)`, `enumerate(items)` | type arguments bound from `__init__`/`__new__` |
| Special forms | `Optional[X]`, `X \| None`, `type[X]`/`Type[X]`, `List`/`Dict`/`Set`/`Tuple`/`DefaultDict`... | X or None, the class X, `builtins`/`collections` classes |
| PEP 695 | `class Stack[S]`, `def last[L](xs: list[L]) -> L` | as `Generic[...]` |

Hover shows the type arguments: `dict[str, Item]`, `tuple[int, Item]`.

## How

- A parameterized type is the evaluation of its class with `ContextKey::TypeArgs`.
- A generic class records its type parameters and the type arguments it gives to its bases
  (`ClassSymbol.generics`, set when its bases are loaded).
- A member found on a generic class gets the values of that class's type variables
  (`ContextKey::TypeBindings`), computed from the receiver along the bases; what the member
  returns is substituted with them. A type variable left unbound gives nothing (Any).
- Overloads are chosen by the call: by arity and `None` parameters for calls, by `slice`
  for `__getitem__`.
- Methods of stubs (typeshed) are evaluated on demand when a generic binding needs them
  (`SyncOdoo::ensure_generic_func_evaluations`); other stub methods are not, as before.

## Limits

- A union of several types (`A | B`, `Union[A, B]`) stays unknown, as before: without the
  narrowing that `isinstance`/`match` give at run time, every call would be checked against
  each of them (on Odoo 20 this gave false OLS01009 on `cryptography` key types and OLS03014
  on `mapped() -> list | BaseModel`). Only `X | None` and `Optional[X]` are modeled.
- Not modeled: ParamSpec, TypeVarTuple, bounds and constraints of type variables, variance,
  `Callable[..., R]`, `Literal`. Structural protocols are matched only through the members
  `__next__`, `__iter__`, `__getitem__`, `__enter__`, `__call__`, and only for a class that
  has every method of the protocol.

## Measurements

`--parse` of the whole of Odoo, average of three runs on a loaded machine:

| | Before | After |
|---|---|---|
| Odoo 20.0, CPU | 14.50 s | 15.28 s (+5%) |
| Odoo 19.0, CPU | 12.90 s | 12.80 s |
| Odoo 20.0, memory | 789 MB | 799 MB (+1.3%) |
| Odoo 19.0 diagnostics | 2666 | 2641 (-28 false OLS01001, 3 messages reworded) |
| Odoo 20.0 diagnostics | 2827 | 2827 (3 messages reworded) |
