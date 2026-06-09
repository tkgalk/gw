use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

/// Directory that `gw clone` is currently populating. If set, it should be
/// removed on interruption (Ctrl-C). Shared with the signal handler thread;
/// `take()` gives mutual exclusion so the handler and the normal error path
/// never both try to remove it.
static CLEANUP_PATH: Mutex<Option<PathBuf>> = Mutex::new(None);

#[derive(Parser)]
#[command(name = "gw", about = "Bare-clone + worktree helper", version)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Clone a repo into a worktree-friendly layout (.bare + default-branch worktree)
    Clone {
        /// Git URL (ssh or https)
        url: String,
        /// Target directory (defaults to repo name in CWD)
        path: Option<PathBuf>,
    },
    /// Create a new repo in the worktree-friendly layout (.bare + initial-branch worktree)
    New {
        /// Target directory (also the repo name)
        path: PathBuf,
        /// Initial branch name (defaults to git's init.defaultBranch, or "main")
        #[arg(short, long)]
        branch: Option<String>,
    },
    /// Manage worktrees in the current bare-clone repo
    Worktree {
        #[command(subcommand)]
        action: WtAction,
    },
}

#[derive(Subcommand)]
enum WtAction {
    /// Add a new worktree (creates a new branch off the default branch by default)
    Add {
        /// Worktree directory name (and new branch name unless --existing)
        name: String,
        /// Base ref to branch off (defaults to repo's default branch)
        #[arg(long)]
        base: Option<String>,
        /// Check out an existing branch instead of creating a new one
        #[arg(long)]
        existing: bool,
    },
    /// Remove a worktree
    Remove {
        /// Worktree directory name
        name: String,
        /// Force removal even with uncommitted changes
        #[arg(short, long)]
        force: bool,
    },
    /// List worktrees
    List,
    /// Prune bookkeeping for worktree directories that were deleted manually
    Prune {
        /// Show what would be pruned without removing anything
        #[arg(short = 'n', long)]
        dry_run: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Cmd::Clone { url, path } => clone(&url, path.as_deref()),
        Cmd::New { path, branch } => new_repo(&path, branch.as_deref()),
        Cmd::Worktree { action } => match action {
            WtAction::Add {
                name,
                base,
                existing,
            } => wt_add(&name, base.as_deref(), existing),
            WtAction::Remove { name, force } => wt_remove(&name, force),
            WtAction::List => wt_list(),
            WtAction::Prune { dry_run } => wt_prune(dry_run),
        },
    }
}

fn clone(url: &str, path: Option<&Path>) -> Result<()> {
    let target: PathBuf = match path {
        Some(p) => p.to_path_buf(),
        None => PathBuf::from(repo_name_from_url(url)?),
    };

    let default_branch = with_clean_target(&target, || clone_inner(url, &target))?;
    print_layout(&target, &default_branch);
    Ok(())
}

fn new_repo(target: &Path, branch: Option<&str>) -> Result<()> {
    let default_branch = with_clean_target(target, || new_inner(target, branch))?;
    print_layout(target, &default_branch);
    Ok(())
}

/// Creates an empty `target` directory, runs `build` to populate it, and cleans
/// up the whole directory if `build` fails or the process is interrupted.
///
/// From the moment we create it, `target` is ours: it didn't exist before
/// (checked here) and `build` writes only inside it. If any step fails, remove
/// the whole directory so we don't leave a half-initialized repo behind. Cleanup
/// failures are reported but never mask the original error.
///
/// An interrupt handler is armed too: on Ctrl-C the terminal sends SIGINT to the
/// whole foreground process group, so the child `git` dies on its own; the
/// handler then removes the half-built directory and exits. Without it the
/// default SIGINT disposition would kill us before any cleanup could run.
fn with_clean_target<T>(target: &Path, build: impl FnOnce() -> Result<T>) -> Result<T> {
    if target.exists() {
        bail!("target directory already exists: {}", target.display());
    }

    std::fs::create_dir_all(target).with_context(|| format!("creating {}", target.display()))?;

    arm_interrupt_cleanup();
    *CLEANUP_PATH.lock().unwrap() = Some(target.to_path_buf());

    match build() {
        Ok(value) => {
            // Success: keep the directory, disarm cleanup first so a late Ctrl-C
            // can't wipe the finished repo.
            CLEANUP_PATH.lock().unwrap().take();
            Ok(value)
        }
        Err(e) => {
            // `take()` races the signal handler; whoever wins removes the dir.
            // If the handler already took it, it is removing + exiting, so we
            // skip removal here.
            if let Some(path) = CLEANUP_PATH.lock().unwrap().take()
                && let Err(rm) = std::fs::remove_dir_all(&path)
            {
                eprintln!(
                    "warning: failed to clean up {} after error: {rm}",
                    path.display()
                );
            }
            Err(e)
        }
    }
}

fn print_layout(target: &Path, default_branch: &str) {
    println!();
    println!("{}/", target.display());
    println!("  .bare/");
    println!("  {default_branch}/");
}

/// Install a Ctrl-C handler (once) that removes the in-progress clone directory
/// and exits. The handler runs on a dedicated thread (not in async-signal
/// context), so `remove_dir_all` is safe to call here.
fn arm_interrupt_cleanup() {
    // `set_handler` errors if a handler is already installed; that's fine since
    // we only need it set once per process.
    let _ = ctrlc::set_handler(|| {
        if let Ok(mut guard) = CLEANUP_PATH.lock()
            && let Some(path) = guard.take()
        {
            let _ = std::fs::remove_dir_all(&path);
            eprintln!("\ninterrupted; cleaned up {}", path.display());
        }
        std::process::exit(130);
    });
}

/// Populates an already-created `target` directory with the bare clone and the
/// default-branch worktree. Returns the default branch name on success.
fn clone_inner(url: &str, target: &Path) -> Result<String> {
    let bare = target.join(".bare");
    let bare_str = path_str(&bare)?;

    run("git", &["clone", "--bare", url, bare_str])?;
    run(
        "git",
        &[
            "--git-dir",
            bare_str,
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    )?;
    run("git", &["--git-dir", bare_str, "fetch", "origin"])?;

    let default_branch = default_branch(&bare)?;
    let worktree_path = target.join(&default_branch);
    run(
        "git",
        &[
            "--git-dir",
            bare_str,
            "worktree",
            "add",
            path_str(&worktree_path)?,
            &default_branch,
        ],
    )?;

    Ok(default_branch)
}

/// Initializes a bare repo in `target/.bare` and adds an empty worktree on a
/// fresh (unborn) initial branch. Returns the initial branch name on success.
fn new_inner(target: &Path, branch: Option<&str>) -> Result<String> {
    let branch = match branch {
        Some(b) => b.to_string(),
        None => configured_default_branch(),
    };
    let bare = target.join(".bare");
    let bare_str = path_str(&bare)?;
    let worktree_path = target.join(&branch);

    // `-b <branch>` records the initial branch as the bare repo's HEAD, which is
    // how a remoteless repo tells `default_branch()` what to branch off later.
    run("git", &["init", "--bare", "-b", &branch, bare_str])?;
    run(
        "git",
        &[
            "--git-dir",
            bare_str,
            "worktree",
            "add",
            "--orphan",
            "-b",
            &branch,
            path_str(&worktree_path)?,
        ],
    )?;

    Ok(branch)
}

/// The branch name `git init` would use: `init.defaultBranch` if configured,
/// otherwise git's built-in default of "master" is overridden here to "main".
fn configured_default_branch() -> String {
    let out = Command::new("git")
        .args(["config", "init.defaultBranch"])
        .output();
    if let Ok(out) = out
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    "main".to_string()
}

fn wt_add(name: &str, base: Option<&str>, existing: bool) -> Result<()> {
    let (root, bare) = find_bare()?;
    let target = root.join(name);
    let bare_str = path_str(&bare)?;
    let target_str = path_str(&target)?;

    if existing {
        run(
            "git",
            &["--git-dir", bare_str, "worktree", "add", target_str, name],
        )?;
    } else {
        let base_branch = match base {
            Some(b) => b.to_string(),
            None => default_branch(&bare)?,
        };
        run(
            "git",
            &[
                "--git-dir",
                bare_str,
                "worktree",
                "add",
                "-b",
                name,
                target_str,
                &base_branch,
            ],
        )?;
    }
    println!("worktree: {}", target.display());
    Ok(())
}

fn wt_remove(name: &str, force: bool) -> Result<()> {
    let (root, bare) = find_bare()?;
    let target = root.join(name);
    let bare_str = path_str(&bare)?;
    let target_str = path_str(&target)?;

    let mut args: Vec<&str> = vec!["--git-dir", bare_str, "worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(target_str);
    run("git", &args)?;
    println!("removed: {}", target.display());
    Ok(())
}

fn wt_list() -> Result<()> {
    let (_, bare) = find_bare()?;
    run("git", &["--git-dir", path_str(&bare)?, "worktree", "list"])
}

fn wt_prune(dry_run: bool) -> Result<()> {
    let (_, bare) = find_bare()?;
    let bare_str = path_str(&bare)?;
    let mut args: Vec<&str> = vec!["--git-dir", bare_str, "worktree", "prune", "--verbose"];
    if dry_run {
        args.push("--dry-run");
    }
    run("git", &args)
}

fn find_bare() -> Result<(PathBuf, PathBuf)> {
    let mut cur = std::env::current_dir().context("getting current directory")?;
    loop {
        let candidate = cur.join(".bare");
        if candidate.is_dir() && candidate.join("HEAD").exists() {
            return Ok((cur, candidate));
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => bail!("no .bare directory found at or above the current directory"),
        }
    }
}

fn default_branch(bare: &Path) -> Result<String> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output()
        .context("running git symbolic-ref")?;

    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(rest) = s.trim().strip_prefix("refs/remotes/origin/") {
            return Ok(rest.to_string());
        }
    }

    // No remote (e.g. a `gw new` repo): fall back to the bare repo's own HEAD,
    // but only once the branch actually exists — an unborn branch has nothing to
    // branch off, so let it fall through and fail below.
    let head = Command::new("git")
        .arg("--git-dir")
        .arg(bare)
        .args(["symbolic-ref", "HEAD"])
        .output()
        .context("running git symbolic-ref HEAD")?;
    if head.status.success() {
        let s = String::from_utf8_lossy(&head.stdout);
        if let Some(rest) = s.trim().strip_prefix("refs/heads/") {
            let exists = Command::new("git")
                .arg("--git-dir")
                .arg(bare)
                .args(["show-ref", "--verify", "--quiet"])
                .arg(format!("refs/heads/{rest}"))
                .status()
                .context("running git show-ref")?;
            if exists.success() {
                return Ok(rest.to_string());
            }
        }
    }

    for candidate in ["main", "master", "trunk"] {
        let chk = Command::new("git")
            .arg("--git-dir")
            .arg(bare)
            .args(["show-ref", "--verify", "--quiet"])
            .arg(format!("refs/heads/{candidate}"))
            .status()
            .context("running git show-ref")?;
        if chk.success() {
            return Ok(candidate.to_string());
        }
    }

    bail!("could not determine default branch (tried symbolic-ref + main/master/trunk)")
}

fn repo_name_from_url(url: &str) -> Result<String> {
    let trimmed = url.trim_end_matches('/');
    let last = trimmed
        .rsplit(['/', ':'])
        .next()
        .filter(|s| !s.is_empty())
        .with_context(|| format!("could not parse repo name from URL: {url}"))?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    if name.is_empty() {
        bail!("could not parse repo name from URL: {url}");
    }
    Ok(name.to_string())
}

fn path_str(p: &Path) -> Result<&str> {
    p.to_str()
        .with_context(|| format!("path is not valid UTF-8: {}", p.display()))
}

fn run(prog: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(prog)
        .args(args)
        .status()
        .with_context(|| format!("spawning `{prog} {}`", args.join(" ")))?;
    if !status.success() {
        bail!("`{prog} {}` failed ({status})", args.join(" "));
    }
    Ok(())
}
