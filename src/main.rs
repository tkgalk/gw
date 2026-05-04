use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::Command;

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

    if target.exists() {
        bail!("target directory already exists: {}", target.display());
    }

    std::fs::create_dir_all(&target).with_context(|| format!("creating {}", target.display()))?;

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

    println!();
    println!("{}/", target.display());
    println!("  .bare/");
    println!("  {}/", default_branch);
    Ok(())
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
