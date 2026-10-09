---
name: git-repo
description: Inspect and operate on git repositories on this machine — status, log, diff, branches, commits. Use for any question about repo state, recent changes, or uncommitted work.
---

# Git Repos

## When to use
- The user asks about a repository's state, recent commits, or pending changes
- The user asks to branch, commit, stash, or diff

## How
Run git through `run_command`. Git is on PATH. Always pass the repo with `-C`
so the command is unambiguous, and quote paths that contain spaces:

```
git -C C:\repos\firstmate status --short
git -C C:\repos\firstmate log --oneline -10
git -C C:\repos\firstmate diff --stat
git -C C:\repos\firstmate branch --show-current
```

## Rules
- If `git` is not on PATH (`where git` finds nothing), Windows git is NOT
  installed: say so and stop. Do NOT parse `.git` internals yourself.
- NEVER run mutating commands (`commit`, `push`, `reset`, `checkout`, `clean`)
  unless the user explicitly asked for that exact action.
- Long output: cap it (`-10`, `--stat`) or pipe to `findstr /i <term>`.
- "Where are my repos?" → `search_files` for a `.git` directory name under the
  likely roots, not a full-disk walk.

## References
- `read_skill_resource("git-repo/references/recipes.md")` for common recipes
  (what changed today, who wrote a line, lost work in the reflog).
