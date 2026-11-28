# Changelog

## [Unreleased] - fork: memory

### Server

- Much lower memory use, see `server/docs/memory.md`.
- New setting `modules_scope` (`"dependencies"` by default, or `"all"`): only the modules the workspace modules need are loaded; the others are described by a persistent module index in the user cache directory.
  Unloaded modules are loaded on demand without a restart: when a file of theirs is opened, when a folder is added to the workspace, and when they become `auto_install`-triggered by a module loaded at runtime. Workspace symbols and model name completion also list the models of unloaded modules.
- New setting `ast_cache_size` (MB, default 64, 0 = unbounded): RAM budget for the ASTs and source texts of files that are not open in the editor (and the texts of XML/CSV/JS files); least recently used ones are rebuilt from disk on demand.
- Python files of Odoo / addons-path modules outside the workspace are no longer validated (their diagnostics were never published).
- `OLS_MEMPROF=<file.csv>`: opt-in memory profiling.
- Python interpreter in a Docker/Podman container, see `server/docs/python-env.md`. New settings `python_command` (the interpreter as a command line, e.g. `docker exec -i odoo python3`; takes precedence over `python_path`) and `path_mappings` (`container_path=host_path` for volumes); CLI `--python-command` and `--path-mapping`.
  The container's `sys.path` directories are copied once per image to the user cache directory (with `tar` in the container: no bytecode, no `odoo` package, empty placeholders for compiled extensions). The environment is cached by image ID: later starts only run `docker inspect`, and the cache is used when the container is unavailable.
- The interpreter is run once per start instead of twice (one probe for the version, `EXTENSION_SUFFIXES` and `sys.path`).
- Generic types, see `server/docs/generics.md`: type arguments are kept (`list[Item]`, `dict[str, Item]`) and substituted in methods, indexing (`x[0]`, `d["k"]`, `.get()`), `for` loops (also `for k, v in d.items()`, `enumerate`) and inherited bases (`class C(Box[Item])`); `Optional[X]`, `X | None`, `type[X]`, typing aliases (`List`, `Dict`...); generic functions and constructors bind their type variables from the arguments; PEP 695 (`class C[T]`, `def f[T]`). Hover shows the type arguments.
- No more false OLS01001 `//Unhandled// not found` on `Generic[...]`, `Protocol[...]` and `List[...]` bases; an unresolved base is reported with its expression (`Unknown[int]`, `a.b`).

## [1.6.2] - 2026/11/04 - Fixes

### Server

- Automatically detect ANSI color support and add a new CLI argument: `--color [mode]`, where `mode` can be `auto`, `always`, or `never`. `auto` is selected by default.
- Add a warning for users of tsserver versions earlier than 6. tsserver 6 is the minimum required version for OdooLS.

### PyCharm

- *We are updating OdooLS's PyCharm dependencies to support recent versions of PyCharm (later than 2026.2). Unfortunately, this means dropping support for older versions. PyCharm 2025.3.1 is now the oldest supported version.*

### Fixes

- Ensure that OdooLS does not stay alive after the client is closed.
- Fix the evaluation of a function annotated with `@api.returns` so that it returns an instance of the specified model.
