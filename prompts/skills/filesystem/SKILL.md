---
name: filesystem
description: Read, edit, write, list, and search files and directories on this machine, and list running processes. Use for any question about file contents, directory state, or what is installed/running.
tools: read_file, edit_file, write_file, list_dir, search_files, list_processes
---

# Filesystem & Processes

Loading this skill enables six tools: `read_file`, `edit_file`, `write_file`,
`list_dir`, `search_files`, `list_processes`. Prefer them over shelling out
through `run_command` — they return clean output without quoting problems,
and `run_command` refuses duplicated patterns (`dir`, `type`, `findstr`, ...)
when these tools are available.

## Tools
- `read_file(path, offset?, limit?)` — numbered lines (`N:TEXT`) with a
  `[path#TAG]` snapshot header. Window big files with `offset`/`limit`;
  the footer tells you the next offset. Binary files are refused.
- `edit_file(path, tag, ops)` — line-anchored edits against a snapshot you
  have read. `tag` is the `TAG` from that file's `[path#TAG]` header; a stale
  tag means the file changed under you — re-read, re-anchor, retry.
  Ops (newline-separated, apply BOTTOM-UP so earlier numbers stay valid):
  - `PUT N.=M:` replace lines N..M with following `+`-prefixed body lines
  - `PUT <N:` / `PUT >N:` insert body before / after line N
  - `CUT N.=M` delete lines N..M
  Body lines are verbatim; a lone `+` is a blank line. The result reports the
  new `[path#TAG]` — line numbers shift, so re-read before another edit.
- `write_file(path, content, tag?)` — create a new file, or replace an
  existing one ENTIRELY. Overwriting a file you have read requires its
  current `tag`; an existing file you never read is refused (read it first —
  you may have been about to destroy content you needed).
- `list_dir(path)` — entries; directories end with `/`.
- `search_files(pattern, root)` — recursive case-insensitive name substring,
  max 200 paths. ALWAYS pass a narrow `root` (e.g. `C:\Users\dusti`);
  the default root is `C:\` and a full drive walk is slow.
- `list_processes()` — CSV of all processes.

## Rules
- Paths are Windows paths (`C:\Users\dusti\notes.txt`).
- Edit workflow: `read_file` -> anchor line numbers from THAT output ->
  `edit_file` with the header's tag. Never guess line numbers.
- To read many files or grep content, use `run_command` with
  `findstr /s /i /m "term" *.md` — `search_files` matches names only.
- To filter processes, `run_command` with `tasklist | findstr /i <name>`
  beats pulling the whole `list_processes()` CSV.
- Writing into another user's profile or `C:\Program Files` needs elevation;
  a permission error means stop and report, not retry.
- Deleting: there is no delete tool. `del`/`Remove-Item` via `run_command`
  only when the user explicitly asked to delete those exact files.
