# Git recipes

## What changed today
```
git -C <repo> log --since=midnight --oneline
git -C <repo> status --short
```

## Uncommitted work at a glance
```
git -C <repo> diff --stat
git -C <repo> diff --cached --stat
```

## Who last touched a line range
```
git -C <repo> blame -L 40,60 -- src/main.rs
```

## Lost work (reset/checkout accidents)
```
git -C <repo> reflog -20
```
Reflog entries are recoverable with `git -C <repo> show <sha>`; only suggest
restoring after the user confirms.

## Is the tree clean enough to switch branches
```
git -C <repo> status --porcelain
```
Empty output = clean. Anything else: report the dirty files, do not stash
without asking.
