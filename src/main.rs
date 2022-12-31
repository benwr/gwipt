use git2::Repository;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use tracing::{debug, error, info};

#[derive(Debug, Serialize)]
struct OpenAiRequest {
    model: String,
    prompt: String,
    suffix: String,
    temperature: f64,
    max_tokens: usize,
    top_p: f64,
    frequency_penalty: f64,
    presence_penalty: f64,
}

#[derive(Debug, Deserialize)]
struct ResponseChoice {
    text: String,
    // index: usize,
    // finish_reason: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    // completion_tokens: usize,
    // total_tokens: usize,
}
#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    // id: String,
    // object: String,
    // created: usize,
    // model: String,
    choices: Vec<ResponseChoice>,
    // usage: Usage,
}

fn get_commit_message(name: String, email: String, diff: String) -> reqwest::Result<String> {
    let now = chrono::Local::now();
    let prefix = format!(
        "Author: {} <{}>\nDate:   {}",
        name,
        email,
        now.format("%a %b %-d %H:%M:%S %Y %z")
    );
    let key =
        std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY environment variable must be set");
    let client = reqwest::blocking::Client::new();
    let prefixlen = prefix.len();
    let request = OpenAiRequest {
        model: "text-davinci-003".to_string(),
        prompt: prefix,
        // The API limits are in terms of tokens. We are allowed (I think) 2048 tokens. Unfortunately,
        // there's no easy way to calculate the number of tokens our prompt contains. This number
        // is currently set to be quite conservative, but still large enough to contain most single-edit
        // changes.
        suffix: diff.chars().take((2048 - 100) * 2 - prefixlen).collect(),
        temperature: 0.7,
        max_tokens: 100,
        top_p: 0.9,
        frequency_penalty: 0.1,
        presence_penalty: 0.0,
    };
    // TODO limit length of diff to ensure no errors
    let response = client
        .post("https://api.openai.com/v1/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", key))
        .json(&request)
        .send()?;
    let response: OpenAiResponse = response.json()?;

    let issue_re =
        regex::Regex::new(r"(\(?(([Ff]ix(es)?)|([Cc]loses?))?\s*#\d+\)?)|([Mm]erge [Pp].*\n)")
            .expect("Regex failed to compile");
    let commit_message = issue_re.replace_all(&response.choices[0].text, "");
    Ok(commit_message
        .trim()
        .split('\n')
        .next()
        .expect("Result of split() had no entries!")
        .to_string())
}

fn prepare_wip_branch(repo: &Repository) -> Result<String, git2::Error> {
    let head_ref = repo.head()?;
    if !head_ref.is_branch() {
        return Err(git2::Error::from_str(
            "You must check out a branch for gwipt to work.",
        ));
    }
    let head_branch_name = head_ref
        .shorthand()
        .ok_or(git2::Error::from_str("Could not get branch name"))?;
    let wip_branch_name = String::from("wip/") + &head_branch_name;
    let head_commit = head_ref.peel_to_commit()?;
    let head_tree = head_commit.tree()?;
    let head_commit_id = head_commit.id();
    let mut existing_wip_branch =
        if let Ok(branch) = repo.find_branch(&wip_branch_name, git2::BranchType::Local) {
            branch
        } else {
            debug!("Branching to {} with {}", &wip_branch_name, head_commit.id());
            repo.branch(&wip_branch_name, &head_commit, true)?
        };
    let existing_wip_commit = existing_wip_branch.get().peel_to_commit()?;
    let existing_wip_commit_id = existing_wip_commit.id();
    let me = repo.signature()?;

    if existing_wip_commit_id != head_commit_id
        && !repo.graph_descendant_of(existing_wip_commit_id, head_commit_id)?
    {
        let message = "Merge HEAD into wip/ branch";
        let new_commit_id = repo.commit(
            Some(&(String::from("refs/heads/") + &wip_branch_name)),
            &me,
            &me,
            message,
            &head_tree,
            &[&existing_wip_commit, &head_commit],
        )?;
        info!("{}: {}", new_commit_id, message);
    }
    Ok(wip_branch_name)
}

fn prepare_diff<'a, 'b>(
    repo: &'a Repository,
    wip_branch_name: &'b str,
) -> Result<(git2::Signature<'a>, git2::Diff<'a>), git2::Error> {
    let wip_branch = repo.find_branch(wip_branch_name, git2::BranchType::Local)?;
    let wip_tree = wip_branch.get().peel_to_tree()?;
    let mut diff_options = git2::DiffOptions::new();
    diff_options.minimal(true).include_untracked(true).recurse_untracked_dirs(true).show_untracked_content(true);
    let diff = repo.diff_tree_to_workdir(Some(&wip_tree), Some(&mut diff_options))?;

    Ok((repo.signature()?, diff))
}

// Copied from https://github.com/rust-lang/git2-rs/blob/master/src/util.rs
#[cfg(unix)]
pub fn bytes2path(b: &[u8]) -> &Path {
    use std::os::unix::prelude::*;
    Path::new(OsStr::from_bytes(b))
}

#[cfg(windows)]
pub fn bytes2path(b: &[u8]) -> &Path {
    use std::str;
    Path::new(str::from_utf8(b).unwrap())
}

enum TreeNode {
    Internal(String),
    Leaf(String, git2::Oid, u32),
}

fn try_commit(
    repo: &Repository,
    wip_branch_name: &str,
    commit_message: &str,
    diff: &git2::Diff,
) -> Result<git2::Oid, git2::Error> {
    // at this point, we have a wip branch ready to go. We need to add everything (other than
    // ignored stuff) in the current working directory to a tree, and commit it to the tip of the
    // wip branch.
    let path = repo
        .path()
        .parent()
        .expect("Git repository does not appear to have a parent dir")
        .to_path_buf();

    let mut index = repo.index()?;
    index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
    let branch = repo.find_branch(wip_branch_name, git2::BranchType::Local)?;
    let result_tree_id = index.write_tree()?;
    let result_tree = repo.find_tree(result_tree_id)?;
    let me = repo.signature()?;
    debug!("branchname: {}", wip_branch_name);
    debug!("parent commit_id: {}", &branch.get().peel_to_commit()?.id());
    debug!("tree_id: {}", result_tree_id);
    repo.commit(
        Some(&(String::from("refs/heads/") + wip_branch_name)),
        &me,
        &me,
        commit_message,
        &result_tree,
        &[&branch.get().peel_to_commit()?],
    )
}

fn handle_change(repo: &Repository) {
    match prepare_wip_branch(repo) {
        Ok(name) => match prepare_diff(repo, &name) {
            Ok((signature, diff)) => {
                let mut diff_lines = vec![String::from("\n\n")];
                match diff.print(git2::DiffFormat::Patch, |_, _, l| {
                    let line = if ['+', '-', ' '].contains(&l.origin()) {
                        format!(
                            "{}{}",
                            l.origin(),
                            std::str::from_utf8(l.content()).unwrap()
                        )
                    } else {
                        format!("{}", std::str::from_utf8(l.content()).unwrap())
                    };
                    diff_lines.push(line);
                    true
                }) {
                    Ok(()) => {
                        if diff_lines.len() <= 1 {
                            debug!("Empty diff");
                            return;
                        }
                        let difftext = diff_lines.join("");
                        match get_commit_message(
                            signature.name().unwrap().to_string(),
                            signature.email().unwrap().to_string(),
                            difftext,
                        ) {
                            Ok(message) => {
                                debug!("Got a commit message");
                                match try_commit(repo, &name, &(String::from("wip: ") + &message), &diff) {
                                    Ok(id) => info!("Commit {}: {}", id, message),
                                    Err(e) => error!("Failed to commit to wip branch: {}", e),
                                }
                            }
                            Err(e) => error!("Could not get commit message: {}", e),
                        }
                    }
                    Err(e) => error!("Could not extract diff lines: {}", e),
                };
            }
            Err(e) => error!("Could not prepare diff: {}", e),
        },
        Err(e) => error!("Could not prepare wip branch: {}", e),
    }
    debug!("Change handler exit");
}

#[derive(Debug)]
enum AppError {
    GitError(git2::Error),
    NotifyError(notify_debouncer_mini::notify::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            AppError::GitError(e) => write!(f, "Git Error: {}", e),
            AppError::NotifyError(e) => write!(f, "File watcher error: {}", e),
        }
    }
}

impl std::error::Error for AppError {}

impl std::convert::From<notify_debouncer_mini::notify::Error> for AppError {
    fn from(e: notify_debouncer_mini::notify::Error) -> Self {
        AppError::NotifyError(e)
    }
}

fn main() -> Result<(), AppError> {
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
    tracing_subscriber::fmt::init();
    let repository = match Repository::discover(".") {
        Ok(r) => r,
        Err(e) => {
            error!("Git error: {}", &e);
            return Err(AppError::GitError(e));
        }
    };
    let path = repository
        .path()
        .parent()
        .expect("Git repository does not appear to have a parent dir")
        .to_path_buf();
    debug!("Found git repository at {}", path.display());

    let mut debouncer = new_debouncer(
        std::time::Duration::new(0, 100_000_000),
        None,
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                debug!("{} events", events.len());
                let any_non_git_files = events.iter().any(|e| {
                    let p = &e.path;
                    !p.components().any(|part| {
                        part == std::path::Component::Normal(std::ffi::OsStr::new(".git"))
                    })
                });
                if any_non_git_files {
                    debug!("Found files not in a .git directory");
                    handle_change(&repository);
                } else {
                    debug!("No files outside of .git changed");
                }
            }
            Err(e) => error!("Error watching files: {:?}", e),
        },
    )?;

    debouncer.watcher().watch(&path, RecursiveMode::Recursive)?;

    debug!("Set up filewatcher");

    loop {}
}
