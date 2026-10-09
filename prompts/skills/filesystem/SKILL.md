---
name: filesystem
description: Read, edit, write, list, and search files and directories on this machine, and list running processes. Use for any question about file contents, directory state, or what is installed/running.
---

# Filesystem & Processes

Six tools cover file and process work: `read_file`, `edit_file`,
`write_file`, `list_dir`, `search_files`, `list_processes`. This skill
loaded them, so they are available now. Prefer them over running the same
work through `run_command`: their output is clean, needs no quoting, and is
not truncated. While this skill is active, `run_command` also refuses the
patterns it duplicates, such as `dir`, `type`, and `findstr`.

## Tools
- `read_file(path, offset?, limit?)` — numbered lines (`N:TEXT`) with a
  `[path#TAG]` snapshot header. Use `offset` and `limit` to window a big
  file. The footer names the next offset. Binary files are refused.
- `edit_file(path, tag, ops)` — line-anchored edits against a snapshot you
  have read. `tag` is the `TAG` from that file's `[path#TAG]` header. A stale
  tag means the file changed under you. Re-read, re-anchor, and retry.
  Ops are newline-separated and apply BOTTOM-UP, so earlier numbers stay
  valid:
  - `PUT N.=M:` replace lines N..M with the following `+`-prefixed body lines.
  - `PUT <N:` / `PUT >N:` insert the body before or after line N.
  - `CUT N.=M` delete lines N..M.
  Body lines are verbatim, and a lone `+` is a blank line. The result reports
  the new `[path#TAG]`. Line numbers shift, so re-read before another edit.
- `write_file(path, content, tag?)` — create a new file, or replace an
  existing one ENTIRELY. Overwriting a file you have read requires its
  current `tag`. A file you never read is refused, because you may be about
  to destroy content you still need. Read it first.
- `list_dir(path)` — the entries, with directories ending in `/`.
- `search_files(pattern, root)` — recursive, case-insensitive name substring
  match, capped at 200 paths. ALWAYS pass a narrow `root`, for example
  `C:\Users\dusti`. The default root is `C:\`, and a full drive walk is
  slow.
- `list_processes()` — CSV of all processes.

## Rules
- Paths are Windows paths (`C:\Users\dusti\notes.txt`).
- Edit workflow: `read_file` first, then take line numbers from THAT output,
  then call `edit_file` with the tag from that header. Never guess line
  numbers.
- To read many files or grep content, use `run_command` with
  `findstr /s /i /m "term" *.md` — `search_files` matches names only.
- To filter processes, `run_command` with `tasklist | findstr /i <name>`
  beats pulling the whole `list_processes()` CSV.
- Writing into another user's profile or `C:\Program Files` needs elevation.
  A permission error means stop and report. Do not retry.
- Deleting: there is no delete tool. Use `del` or `Remove-Item` through
  `run_command` only when the user explicitly asked to delete those exact
  files.
