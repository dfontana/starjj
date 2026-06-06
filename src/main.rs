use std::{
    collections::{BTreeMap, HashSet},
    fmt::Write as _,
    io::Write as _,
    process::ExitCode,
};

use jj_cli::{
    cli_util::{CliRunner, CommandHelper, RevisionArg, WorkspaceCommandHelper},
    command_error::CommandError,
    diff_util::{DiffStatOptions, DiffStats, get_copy_records},
    ui::Ui,
};
use jj_lib::{
    backend::CommitId,
    commit::Commit,
    conflicts::ConflictMarkerStyle,
    copies::CopyRecords,
    fileset::FilesetExpression,
    object_id::ObjectId as _,
    repo::{ReadonlyRepo, Repo},
    view::View,
};

// --- Rendering constants -----------------------------------------------------
// Defaults mirror the original starship-jj. Configuration points come later.

const RESET: &str = "\x1b[0m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";

/// Text printed between modules and between bookmarks.
const SEP: &str = " ";
/// Suffix shown when a bookmark is behind the working copy.
const BEHIND_SYMBOL: char = '\u{21e1}'; // ⇡
/// How far back through ancestors we look for bookmarks.
const SEARCH_DEPTH: usize = 100;

#[derive(clap::Parser, Clone, Debug)]
enum Command {
    /// Print the jj prompt segment.
    Prompt,
}

async fn run(_ui: &mut Ui, helper: &CommandHelper, _command: Command) -> Result<(), CommandError> {
    // Never let a prompt-line tool emit errors: anything goes wrong (not a jj
    // repo, corrupt state, …) and we print nothing instead of polluting stderr.
    let out = gather(helper).await.unwrap_or_default();
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
    Ok(())
}

async fn gather(helper: &CommandHelper) -> Result<String, CommandError> {
    // Always skip the (slow) working-copy snapshot: this tool is only ever
    // invoked with `--ignore-working-copy`.
    let ws = helper.workspace_helper_no_snapshot(&Ui::null()).await?;

    let repo = ws.repo().clone();
    let view = repo.view();

    let mut out = String::new();

    let Some(wc_id) = view.get_wc_commit_id(ws.workspace_name()).cloned() else {
        // Headless workspace: nothing to show.
        return Ok(out);
    };
    let commit = repo.store().get_commit(&wc_id)?;

    // Bookmarks in the working copy's ancestry.
    let mut bookmarks = BTreeMap::new();
    find_parent_bookmarks(&commit, 0, &mut bookmarks, view, &mut HashSet::new())?;
    push_bookmarks(&mut out, &bookmarks);

    // Working-copy trees, used by both the state and metrics modules.
    let tree = commit.tree();
    let parent_tree = commit.parent_tree(repo.as_ref()).await?;
    let empty = tree.tree_ids() == parent_tree.tree_ids();

    // State warnings.
    push_state(&mut out, &ws, &repo, &commit, &wc_id, empty)?;

    // Diff metrics.
    push_metrics(&mut out, &repo, &commit, &parent_tree, &tree).await?;

    if !out.is_empty() {
        out.push_str(RESET);
    }
    Ok(out)
}

/// Walk ancestors collecting local bookmarks and how far behind the working
/// copy each one is. Stops at the first bookmark found on a branch.
fn find_parent_bookmarks(
    commit: &Commit,
    distance: usize,
    bookmarks: &mut BTreeMap<String, usize>,
    view: &View,
    visited: &mut HashSet<CommitId>,
) -> Result<(), CommandError> {
    if !visited.insert(commit.id().clone()) {
        return Ok(());
    }

    let mut found = false;
    for (name, _) in view.local_bookmarks_for_commit(commit.id()) {
        found = true;
        bookmarks
            .entry(name.as_str().to_string())
            .and_modify(|v| *v = (*v).min(distance))
            .or_insert(distance);
    }
    if found {
        return Ok(());
    }

    if distance >= SEARCH_DEPTH {
        return Ok(());
    }

    let store = commit.store();
    for parent in commit.parent_ids() {
        let parent = store.get_commit(parent)?;
        find_parent_bookmarks(&parent, distance + 1, bookmarks, view, visited)?;
    }
    Ok(())
}

fn push_bookmarks(out: &mut String, bookmarks: &BTreeMap<String, usize>) {
    if bookmarks.is_empty() {
        return;
    }

    // Order by distance (closest first), then by name. `bookmarks` already
    // iterates name-ascending, so a stable sort by distance gives both.
    let mut ordered: Vec<(&String, &usize)> = bookmarks.iter().collect();
    ordered.sort_by_key(|(_, behind)| **behind);

    out.push_str(MAGENTA);
    for (i, (name, behind)) in ordered.iter().enumerate() {
        if i > 0 {
            out.push_str(SEP);
        }
        out.push('"');
        out.push_str(name);
        out.push('"');
        if **behind != 0 {
            out.push(BEHIND_SYMBOL);
            out.push_str(&behind.to_string());
        }
    }
    out.push_str(SEP);
}

fn push_state(
    out: &mut String,
    ws: &WorkspaceCommandHelper,
    repo: &ReadonlyRepo,
    commit: &Commit,
    wc_id: &CommitId,
    empty: bool,
) -> Result<(), CommandError> {
    let conflict = commit.has_conflict();

    // Resolve the change id to detect hidden/divergent commits. Hidden means no
    // visible target remains; divergent means more than one.
    let (hidden, divergent) = match repo.resolve_change_id(commit.change_id())? {
        Some(resolved) => (
            resolved.visible_with_offsets().next().is_none(),
            resolved.is_divergent(),
        ),
        None => (true, false),
    };

    let immutable = is_immutable(ws, wc_id)?;

    let mut parts: Vec<(&str, &str)> = Vec::new();
    if conflict {
        parts.push((RED, "(CONFLICT)"));
    }
    if divergent {
        parts.push((CYAN, "(DIVERGENT)"));
    }
    if hidden {
        parts.push((YELLOW, "(HIDDEN)"));
    }
    if immutable {
        parts.push((YELLOW, "(IMMUTABLE)"));
    }
    if empty {
        parts.push((YELLOW, "(EMPTY)"));
    }

    if parts.is_empty() {
        return Ok(());
    }
    for (i, (color, text)) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str(SEP);
        }
        out.push_str(color);
        out.push_str(text);
    }
    out.push_str(SEP);
    Ok(())
}

/// Is the working-copy commit immutable? Intersect `immutable()` with the
/// single commit so the revset engine short-circuits, rather than materialising
/// the entire immutable history and scanning it in Rust on every render.
fn is_immutable(ws: &WorkspaceCommandHelper, wc_id: &CommitId) -> Result<bool, CommandError> {
    let revset = ws.parse_revset(
        &Ui::null(),
        &RevisionArg::from(format!("immutable() & {}", wc_id.hex())),
    )?;
    Ok(!revset.evaluate()?.is_empty())
}

async fn push_metrics(
    out: &mut String,
    repo: &ReadonlyRepo,
    commit: &Commit,
    parent_tree: &jj_lib::merged_tree::MergedTree,
    tree: &jj_lib::merged_tree::MergedTree,
) -> Result<(), CommandError> {
    let store = repo.store();
    let matcher = FilesetExpression::all().to_matcher();

    let mut copy_records = CopyRecords::default();
    for parent in commit.parent_ids() {
        let records = get_copy_records(store, parent, commit.id(), &matcher).await?;
        copy_records.add_records(records);
    }

    let tree_diff = parent_tree.diff_stream_with_copies(tree, &matcher, &copy_records);
    let stats = DiffStats::calculate(
        store,
        tree_diff,
        &DiffStatOptions::default(),
        ConflictMarkerStyle::Diff,
    )
    .await?;

    let files = stats.entries().len();
    let added = stats.count_total_added();
    let removed = stats.count_total_removed();

    // Template: "[{changed} {added}{removed}]" with default colours.
    let _ = write!(
        out,
        "{MAGENTA}[{CYAN}{files}{MAGENTA} {GREEN}+{added}{RED}-{removed}{MAGENTA}]{SEP}"
    );
    Ok(())
}

fn main() -> ExitCode {
    CliRunner::init()
        .name("starjj")
        .version(env!("CARGO_PKG_VERSION"))
        .add_subcommand(run)
        .run()
        .into()
}
