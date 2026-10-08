---
name: filesystem
description: Read, write, list, and search files and directories on this machine, and list running processes. Use for any question about file contents, directory state, or what is installed/running.
tools: read_file, write_file, list_dir, search_files, list_processes
---

# Filesystem & Processes

Loading this skill enables five tools: `read_file`, `write_file`, `list_dir`,
`search_files`, `list_processes`. Prefer them over shelling out through
`run_command` — they return clean, capped output without quoting problems.

## Tools
- `read_file(path)` — file contents.
- `write_file(path, content)` — overwrite/create; parent dir must exist.
- `list_dir(path)` — entries; directories end with `/`.
- `search_files(pattern, root)` — recursive case-insensitive name substring,
  max 200 paths. ALWAYS pass a narrow `root` (e.g. `C:\Users\dusti`);
  the default root is `C:\` and a full drive walk is slow.
- `list_processes()` — CSV of all processes.

## Rules
- Paths are Windows paths (`C:\Users\dusti\notes.txt`).
- To read many files or grep content, use `run_command` with
  `findstr /s /i /m "term" *.md` — `search_files` matches names only.
- To filter processes, `run_command` with `tasklist | findstr /i <name>`
  beats pulling the whole `list_processes()` CSV.
- Writing into another user's profile or `C:\Program Files` needs elevation;
  a permission error means stop and report, not retry.
- Deleting: there is no delete tool. `del`/`Remove-Item` via `run_command`
  only when the user explicitly asked to delete those exact files.
