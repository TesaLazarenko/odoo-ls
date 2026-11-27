# Memory model of the server

This fork targets several editors open at once, each with its own OdooLS process on a
large project (Odoo Community + Enterprise + many custom modules), on a machine with
about 8 GB of free RAM. The rule it follows: **only data that is used often or was used
recently lives in RAM; the bulk lives on disk and is reloaded on demand.**

## Tiers

| Data | Where it lives | Reloaded from | Config |
|---|---|---|---|
| Symbol table (classes, functions, variables, models, XML ids…) of the **loaded** modules | RAM, compacted after each build phase | — | — |
| Parsed Python ASTs and source texts of files **not open** in the editor, and the texts of XML/CSV/JS files (their symbols stay) | RAM, one bounded LRU cache | the source file (re-read, and re-parse for Python, deterministic) | `ast_cache_size` (MB, default 64, 0 = unbounded) |
| ASTs/texts of files **open** in the editor | RAM, never evicted (their content comes from the editor) | — | — |

### Why the cold tier of ASTs is the source file, not a serialized AST

The source is the most compact serialization of an AST. On Odoo 20.0 the 62.5 MB of Python
source parse into 582 MB of `IndexedModule` (ruff 0.17: nodes, 9.7 M tokens of 12 bytes,
7.5 M node-index entries of 16 bytes), i.e. ~9.3 bytes of AST per byte of source. A
serialized-AST disk cache would be several times larger on disk than the sources and
would still have to rebuild all the pointers on load; ruff parses the whole of Odoo in
about a second. Re-parsing is also what the server already did with `file_cache = false`.

## Results

Odoo 20.0 Community, `odoo_ls_server --parse`, 14 cores.

| Scenario | Version | Peak RSS | Retained at end (jemalloc `allocated`) | Time |
|---|---|---:|---:|---:|
| Whole Odoo as workspace | 1.6.2 | 1800 MiB | 1771 MiB | 10.3 s |
| Whole Odoo as workspace | this fork, 64 MB cache | 827 MiB | 662 MiB | 10.4 s |
| `sale` as workspace over all of Odoo | 1.6.2 | 1785 MiB | 1752 MiB | 9.6 s |
| `sale` as workspace over all of Odoo | this fork, 128 MB cache | 838 MiB | 644 MiB | 3.3 s |

Diagnostics are identical to 1.6.2 for Python, XML and CSV on the whole of Odoo 17.0, 18.0,
19.0 and 20.0 (the oxc JavaScript linter itself changed with the dependency update).

## Other measures

- `SymbolTable::shrink_to_fit` after build phases: building pushes into vectors that grow by
  doubling and maps that keep their high-water mark (evaluations had 3× their length in
  capacity). ~40 ms on all of Odoo 20.0.
- `SectionSymbols`: the per-name section map of a body is stored inline (a name is nearly
  always declared once, in one section). It replaced a `HashMap<u32, Vec<SymbolKey>>` of
  ~200 bytes per name.
- Bounded pre-parse read-ahead (96 MB estimated): the parsing workers wait for the build thread instead
  of holding the ASTs of many upcoming modules.
- Python files of Odoo / addons-path modules that are outside the workspace and not open are
  not validated: validation only produces diagnostics, and these are never published.
- jemalloc returns freed pages to the OS after 1 s (`dirty_decay_ms:1000,muzzy_decay_ms:0`,
  overridable with `_RJEM_MALLOC_CONF`), and all arenas are purged right after the initial
  build (an idle server does not allocate enough to drive the decay of the build threads'
  arenas).

## Measuring

`OLS_MEMPROF=<file.csv> odoo_ls_server [--parse …]` writes jemalloc statistics every 200 ms
and at each build phase (LSP mode too); with `--parse` it then frees the state part by part
at the end of the run to attribute the retained memory (`server/src/memprof.rs`). The test suite can be run with a
given AST budget with `OLS_TEST_AST_CACHE_SIZE=<MB>`.
