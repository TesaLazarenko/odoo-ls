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
| Modules that the workspace does **not** need | disk only, described by the persistent module index | loaded on demand (import, new dependency, file opened, workspace folder added, `auto_install` triggered) | `modules_scope = "dependencies"` (default) |
| Module index (manifests, `depends`, `auto_install`, models declared per module) | disk: `<user cache dir>/odoo-ls/module-index/<hash of addons paths>.json`, revalidated per module from file sizes and mtimes (not persisted when an addons path is in the temp dir) | rescanned for changed modules only | built when `modules_scope = "dependencies"` (default) |

### Why the cold tier of ASTs is the source file, not a serialized AST

The source is the most compact serialization of an AST. On Odoo 20.0 the 62.5 MB of Python
source parse into 582 MB of `IndexedModule` (ruff 0.17: nodes, 9.7 M tokens of 12 bytes,
7.5 M node-index entries of 16 bytes), i.e. ~9.3 bytes of AST per byte of source. A
serialized-AST disk cache would be several times larger on disk than the sources and
would still have to rebuild all the pointers on load; ruff parses the whole of Odoo in
about a second. Re-parsing is also what the server already did with `file_cache = false`.

### What a module-level disk tier buys

A typical custom project depends on a fraction of the addons paths: the dependency closure
of `sale` is 23 modules out of the 669 of Odoo 20.0 Community. With
`modules_scope = "dependencies"` only that closure (plus the `auto_install` modules it
triggers) is loaded; the rest stays on disk. The module index keeps what the server must
know about the unloaded modules:

- which modules exist and what they depend on, to compute the closure without loading them;
- which module declares a model, so that a reference to a model of an unloaded module is
  still reported as "not in the dependencies of your module" (OLS03001) and not as an
  unknown model (OLS03002);
- where each model is declared (file and line), so that workspace symbols (`"crm.lead"`) and
  model name completion (`env['crm.`, "require crm") also list the models of unloaded modules.

Modules outside the closure are loaded when needed, without a restart:

- a module that becomes a dependency (manifest edit) or that is imported (`odoo.addons.x`) is
  created on the fly, as upstream (`create_module_from_name`);
- opening a file of an unloaded module (`handle_did_open`, also for the files opened while
  indexing) loads that module, so the file gets its module context instead of becoming a
  custom entry point;
- a folder added to the workspace (`didChangeWorkspaceFolders`) loads the closure of its
  modules;
- after any of these, the `auto_install` modules that the loaded set now triggers are loaded
  too, in the same rebuild (`SyncOdoo::load_triggered_auto_install`, called by
  `process_rebuilds`): adding `crm` to the dependencies of a module that depends on `sale`
  loads `sale_crm`, as Odoo would install it.

Modules are never unloaded at runtime: a removed dependency or workspace folder keeps its
modules in memory until the next restart (diagnostics follow the new manifest at once).

Rules of the closure, as Odoo installs modules: `depends`, recursively, plus `base`; then the
modules whose `auto_install` trigger (all their `depends`, or the listed modules) is in the
set, repeated until stable. Localizations (a non-empty `countries` key) are not followed:
Odoo auto-installs them only for a company of one of these countries. Without that rule the
three custom modules below pulled 247 modules instead of 80. When every module of the addons
paths is in the workspace (e.g. the workspace is Odoo itself) or none is, everything is loaded.

What changes for diagnostics: a module only sees what is loaded. A field or method added to a
model by a module that is neither in the closure nor loaded since (for example `sale_crm`'s
`opportunity_id` on `sale.order` for a module that depends on `sale` only) is not known, so the
related "only defined through indirect inheritance" warnings (OLS03028) are not raised. This is
the price of not keeping every module in memory. `modules_scope = "all"` restores the
previous behaviour.

## Results

Odoo 20.0 Community, `odoo_ls_server --parse`, 14 cores. "Custom" = 3 copies of Odoo modules
renamed (`x_sale_margin`, `x_purchase_stock`, `x_hr_expense`) plus a small test module, as the
workspace, over all of Odoo (addons path).

| Scenario | Version | Peak RSS | Retained at end (jemalloc `allocated`) | Time |
|---|---|---:|---:|---:|
| Whole Odoo as workspace | 1.6.2 | 1800 MiB | 1771 MiB | 10.3 s |
| Whole Odoo as workspace | this fork, 64 MB cache (all modules are in the workspace) | 827 MiB | 662 MiB | 10.4 s |
| `sale` as workspace over all of Odoo | 1.6.2 | 1785 MiB | 1752 MiB | 9.6 s |
| `sale` as workspace over all of Odoo | this fork, `modules_scope = "all"`, 128 MB cache | 838 MiB | 644 MiB | 3.3 s |
| Custom modules over all of Odoo | this fork, `modules_scope = "all"`, 128 MB cache | 825 MiB | 647 MiB | 3.0 s |
| Custom modules over all of Odoo | this fork, defaults (`dependencies`, 64 MB cache) | 525 MiB | 276 MiB | 1.7 s |

Diagnostics are identical to 1.6.2 for Python, XML and CSV on the whole of Odoo 17.0, 18.0,
19.0 and 20.0 (the oxc JavaScript linter itself changed with the dependency update). With
`modules_scope = "dependencies"`, references to modules that are not dependencies can differ
(see above): on the custom modules, one more (justified) error, a group of `hr_expense`
referenced by a copy of `hr_expense` that does not depend on it.

### Eight editors at once (LSP mode)

`.work/bin/multi_lsp.py` (not in the repository) starts 8 servers over stdio at the same time,
one per project, each project being 1–2 copies of Odoo modules over all of Odoo (2 projects per
version, 17.0 to 20.0, each with its own `odools.toml`), waits for the end of indexing, opens a
model file and hovers in it. Memory is RSS + swap of each server.

| | 1.6.2 (measured on 3 of the projects) | this fork, defaults (8 projects) |
|---|---:|---:|
| Indexing time per server, all started together | 10.7–12.7 s | 1.8–4.1 s |
| Peak per server | 1453–1781 MiB | 407–480 MiB |
| Steady state per server, after indexing | 1435–1757 MiB | 299–333 MiB |
| Steady state, 8 servers | ~12.7 GiB (extrapolated, does not fit on the 12 GiB test machine) | 2.5 GiB (measured) |

### Eight large projects: Community + Enterprise-sized addons + 100 custom modules each

Enterprise sources were not available, so a proxy of the same size was used
(`.work/bin/make_big.py`, not in the repository): a second addons path with one `ent_<m>` per
Community addon `<m>` (653 modules, files hard-linked, each depending on `<m>` and on the `ent_`
versions of its dependencies), and 8 projects of 100 custom modules each (different random
slices of Odoo, each module depending on `<m>` and `ent_<m>`). 1422 modules in the addons paths;
a project loads 583 of them. Odoo 20.0, LSP mode, 8 servers started together.

| | 1.6.2 (1 server) | this fork, defaults (8 servers) |
|---|---:|---:|
| Indexing time | 25.5 s | 10.2–12.2 s (all 8 at once) |
| Peak per server | 3244 MiB | 607–644 MiB |
| Steady state per server | 3243 MiB | 558–603 MiB |
| 8 servers | ~25 GiB (extrapolated) | peak 4.8 GiB, steady 4.5 GiB (measured) |

Diagnostics of such a project (9454) are identical with the text cache on and off.

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
  arenas): 508 → 348 MiB resident at the end of indexing on one of the projects above.

## Measuring

`OLS_MEMPROF=<file.csv> odoo_ls_server [--parse …]` writes jemalloc statistics every 200 ms
and at each build phase (LSP mode too); with `--parse` it then frees the state part by part
at the end of the run to attribute the retained memory (`server/src/memprof.rs`). The test suite can be run with a
given AST budget with `OLS_TEST_AST_CACHE_SIZE=<MB>`.
