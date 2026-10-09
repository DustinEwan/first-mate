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

Recover a reflog entry with `git -C <repo> show <sha>`. Only after the user
confirms, suggest that recovery.

## Is the tree clean enough to switch branches

```
git -C <repo> status --porcelain
```

Empty output means the tree is clean. Any other output lists dirty files.
Report those files, and do not stash without asking the user.
