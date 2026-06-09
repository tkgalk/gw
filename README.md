# gw

A bare-clone + worktree helper for Git. Keeps every branch in its own
directory so you can switch contexts without `git stash`.

```
myrepo/
  .bare/          ← the actual git objects
  main/           ← worktree for main
  my-feature/     ← worktree for my-feature
```

## Installation

### Download a pre-built binary

Pick the right binary for your platform from the
[latest release](https://github.com/tkgalk/gw/releases/latest):

| Platform          | Binary                          |
| ----------------- | ------------------------------- |
| macOS (Apple Silicon) | `gw-aarch64-apple-darwin`   |
| macOS (Intel)     | `gw-x86_64-apple-darwin`        |
| Linux x86_64      | `gw-x86_64-unknown-linux-gnu`   |
| Linux arm64       | `gw-aarch64-unknown-linux-gnu`  |

```sh
# Example: macOS Apple Silicon
curl -Lo gw https://github.com/tkgalk/gw/releases/latest/download/gw-aarch64-apple-darwin
chmod +x gw
mv gw /usr/local/bin/
```

### Build from source (requires Rust)

```sh
cargo install --git https://github.com/tkgalk/gw
```

## Usage

### Clone a repo

```sh
gw clone git@github.com:org/repo.git
# creates: repo/.bare/ and repo/<default-branch>/
```

### Create a new repo

```sh
gw new myrepo
# creates: myrepo/.bare/ and myrepo/main/ (an empty worktree, no commits yet)

# Pick the initial branch name (defaults to git's init.defaultBranch, else "main")
gw new myrepo --branch trunk
```

Like `git init`, this leaves you with an empty worktree and no commits — add a
remote yourself once you have one (`git remote add origin <url>`). Requires
git ≥ 2.42 (for `git worktree add --orphan`).

### Manage worktrees

```sh
# Add a new worktree (creates branch off default branch)
gw worktree add my-feature

# Add off a specific base
gw worktree add my-feature --base origin/main

# Check out an existing branch
gw worktree add main --existing

# List all worktrees
gw worktree list

# Remove a worktree
gw worktree remove my-feature

# Prune stale bookkeeping (dry-run first)
gw worktree prune --dry-run
gw worktree prune
```

Run from any directory inside a `gw`-managed repo — it walks up to find the `.bare/` directory automatically.
