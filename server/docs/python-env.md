# Python interpreter in a Docker container

Odoo often runs in a container, and its Python libraries are installed only there. The
server needs three things from the interpreter: its version, its `EXTENSION_SUFFIXES` and
the directories of its `sys.path` to resolve imports. It can take them from an interpreter
in a container without slowing down: the container is entered once per image, never per
request.

## Configuration

```toml
[[config]]
name = "default"
odoo_path = "/home/me/src/odoo"            # Odoo itself must be on this machine
addons_paths = ["${workspaceFolder}/addons"]
python_command = "docker exec -i odoo python3"
# Optional: sys.path directories that are volumes, as container_path=host_path
path_mappings = ["/mnt/extra-addons=${workspaceFolder}/addons"]
```

- `python_command`: the interpreter as a command line, split like a shell would
  (quotes, backslash). It takes precedence over `python_path`. Any command works
  (a wrapper script, `ssh host python3`…); `docker exec` and `podman exec` get the
  container handling below.
- `path_mappings`: `container_path=host_path` entries. The host part is resolved like any
  path setting (`${workspaceFolder}`, relative to the config file) and must be a directory.
  A `sys.path` entry is mapped through the longest matching container prefix.

In CLI mode: `--python-command "docker exec -i odoo python3"` and
`--path-mapping /mnt/extra-addons=/home/me/addons` (repeatable).

## What happens at start

1. **One probe.** The interpreter runs one script that prints, as one JSON line, its
   version, `EXTENSION_SUFFIXES`, the `sys.path` directories and their entries. This is
   also the case for a local interpreter (it used to be run twice).
2. **Container directories.** For `docker exec`/`podman exec`, each `sys.path` directory is
   - read at its host path when `path_mappings` covers it;
   - otherwise copied once per image to `<user cache dir>/odoo-ls/python-env/images/<image id>/fs/<container path>`.
     The copy runs `tar` in the container, without `__pycache__`, `*.pyc` and the `odoo`
     package (Odoo is read from `odoo_path`; in an official image it is 1.3 of the 1.4 GB
     of `dist-packages`). Compiled extensions become empty files: the server only checks
     that they exist. Without `tar` in the image, it falls back to `docker cp`, which
     streams everything and filters on this side. A copy is written to a temporary
     directory and renamed when complete, so an interrupted copy is never used.
3. **Cache.** The result is cached in `<user cache dir>/odoo-ls/python-env/envs/<hash of the
   command and mappings>.json` with the container's image ID. On the next start the server
   runs `docker inspect` (it does not enter the container) and, while the image ID is
   unchanged, uses the cache. When the image changes, the probe and the copy run again and
   copies no cached environment refers to are removed. When Docker or the container is
   unavailable, the cached environment is used as is.

Only the start is concerned: afterwards the server reads the copies like any local
directory, and no request runs anything in the container.

## Measurements

Odoo 18.0 image (`dist-packages` 1.5 GB of which `odoo` 1.3 GB), `--parse` of one custom
module that imports a library installed only in the container:

| Interpreter | Time | Import of the container-only library |
|---|---|---|
| local `/usr/bin/python3` | 0.96 s | `OLS02001 ctronly not found` |
| `docker exec`, first start (probe + copy, 85 MB) | 1.47 s | resolved |
| `docker exec`, next starts (`docker inspect` only) | 0.96 s | resolved |
| `docker exec`, container stopped (cache) | 0.96 s | resolved |

With `docker cp` instead of `tar` in the container, the first start took 24 s and the
copy 1.4 GB before the filters were applied in the container.

## Limits

- Odoo and the addons must be on this machine (`odoo_path`, `addons_paths`): only the
  `sys.path` directories are taken from the container.
- `docker compose exec` is not recognized (it targets a service, not a container): use
  `docker exec <container>` or a wrapper script (then nothing is cached, the probe runs at
  each start, and only `sys.path` entries that exist here or are mapped are used).
- The cache follows the image ID: a package installed by hand in a running container
  (`docker exec ... pip install`) is not seen until the image changes. Remove
  `<user cache dir>/odoo-ls/python-env` to force a new probe and copy.
- Symbolic links of the container directories are not recreated in the copies (a link to
  a directory outside the copy would let a later entry write through it).
- Files that are not UTF-8 (a few old libraries, e.g. `xlwt`) are skipped, as they are
  for a local interpreter.
