# Changelog

## [Unreleased] - fork: memory

### Server

- Much lower memory use, see `server/docs/memory.md`.
- New setting `ast_cache_size` (MB, default 64, 0 = unbounded): RAM budget for the ASTs and source texts of files that are not open in the editor (and the texts of XML/CSV/JS files); least recently used ones are rebuilt from disk on demand.
- Python files of Odoo / addons-path modules outside the workspace are no longer validated (their diagnostics were never published).
- `OLS_MEMPROF=<file.csv>`: opt-in memory profiling.

## [1.6.2] - 2026/11/04 - Fixes

### Server

- Automatically detect ANSI color support and add a new CLI argument: `--color [mode]`, where `mode` can be `auto`, `always`, or `never`. `auto` is selected by default.
- Add a warning for users of tsserver versions earlier than 6. tsserver 6 is the minimum required version for OdooLS.

### PyCharm

- *We are updating OdooLS's PyCharm dependencies to support recent versions of PyCharm (later than 2026.2). Unfortunately, this means dropping support for older versions. PyCharm 2025.3.1 is now the oldest supported version.*

### Fixes

- Ensure that OdooLS does not stay alive after the client is closed.
- Fix the evaluation of a function annotated with `@api.returns` so that it returns an instance of the specified model.
