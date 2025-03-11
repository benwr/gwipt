// Copyright 2023 The gwipt Authors, except as waived below
// 
// Licensed under the CC0 Universal 1.0 License (the "CC0 License"), or the Apache License, Version
// 2.0 (the "Apache License"), at the licensee's discretion. You may obtain a copy of the CC0
// License at
// 
//     https://creativecommons.org/publicdomain/zero/1.0/legalcode
//
// You may obtain a copy of the Apache License at
//
//     https://www.apache.org/licenses/LICENSE-2.0    
//
// Unless required by applicable law or agreed to in writing, this software is distributed on an
// "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
// Licenses for the specific language governing permissions and limitations under the Licenses.
use backoff::ExponentialBackoff;
use clap::Parser;
use git2::Repository;
use llm::chat::{Tool, ParametersSchema, ParameterProperty, ParameterType, ChatMessage, Role};
use time::macros::format_description;
use tracing::{debug, error, info};

const COMMIT_TOOL: Tool = Tool {
    name: "write_commit_message",
    description: "Generate a commit message based on code changes",
    parameters: ParametersSchema {
        schema_type: "object".to_string(),
        properties: [(
            "message".to_string(),
            ParameterProperty {
                description: "Clear, concise one-line commit message summarizing the changes".to_string(),
                schema_type: ParameterType::String,
            },
        )]
        .into_iter()
        .collect(),
        required: vec!["message".to_string()],
    },
};

#[derive(Debug)]
enum CommitMessageError {
    LLMError(llm::error::LLMError),
    TimeError(time::error::IndeterminateOffset),
    TimeFormatError(time::error::Format),
    MissingApiKey,
    MissingToolCall,
    InvalidToolArguments,
}

impl std::fmt::Display for CommitMessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            CommitMessageError::LLMError(e) => write!(f, "LLM error: {}", e),
            CommitMessageError::TimeError(e) => write!(f, "Time error: {}", e),
            CommitMessageError::TimeFormatError(e) => write!(f, "Time formatting error: {}", e),
            CommitMessageError::MissingApiKey => {
                write!(f, "OPENAI_API_KEY environment variable is not set.")
            },
            CommitMessageError::MissingToolCall => {
                write!(f, "LLM response did not include the expected tool call.")
            },
            CommitMessageError::InvalidToolArguments => {
                write!(f, "LLM tool call had invalid or missing arguments.")
            }
        }
    }
}

impl std::error::Error for CommitMessageError {}

impl std::convert::From<llm::error::LLMError> for CommitMessageError {
    fn from(e: llm::error::LLMError) -> Self {
        CommitMessageError::LLMError(e)
    }
}

impl std::convert::From<time::error::IndeterminateOffset> for CommitMessageError {
    fn from(e: time::error::IndeterminateOffset) -> Self {
        CommitMessageError::TimeError(e)
    }
}

impl std::convert::From<time::error::Format> for CommitMessageError {
    fn from(e: time::error::Format) -> Self {
        CommitMessageError::TimeFormatError(e)
    }
}

fn get_message(
    name: String,
    email: String,
    diff: String,
    offset: time::UtcOffset,
) -> Result<String, CommitMessageError> {
    let now = time::OffsetDateTime::now_utc().replace_offset(offset);
    let system_prompt = format!(
        "You are a expert software engineer writing a git commit message.
The user will provide a diff showing changes.
Write a one-line commit message in the conventional style.
The message should:
- Start with a verb in imperative tense
- Be under 72 characters
- Use the format: <prefix>: <description>
Common prefixes: fix, feat, docs, style, refactor, test, chore

Author: {name} <{email}>
Date: {}",
        now.format(format_description!(
            "[weekday repr:short] [month repr:short] [day padding:none] \
                [hour]:[minute]:[second] [year] [offset_hour sign:mandatory][offset_minute]"
        ))?
    );

    debug!("Using system prompt: {}", &system_prompt);
    let key = std::env::var("OPENAI_API_KEY").map_err(|_| CommitMessageError::MissingApiKey)?;
    
    let mut messages = vec![ChatMessage {
        role: Role::System,
        content: system_prompt,
    }];

    messages.push(ChatMessage {
        role: Role::User,
        content: format!("Diff:\n{}", diff),
    });

    let client = llm::backends::openai::OpenAI::new(
        key,
        Some("gpt-3.5-turbo".to_string()),
        None,
        Some(0.7),
        None,
        None,
        None,
        Some(1.0),
        None,
        None,
        None,
        Some(vec![COMMIT_TOOL.clone()]),
        None
    );

    let response = client.chat_with_tools(&messages, Some(&[COMMIT_TOOL]))?;
    
    // Extract the tool call from the response
    let tool_calls = response.tool_calls().ok_or(CommitMessageError::MissingToolCall)?;
    let tool_call = tool_calls.iter()
        .find(|tc| tc.function.name == "write_commit_message")
        .ok_or(CommitMessageError::MissingToolCall)?;

    // Parse the arguments as JSON
    let args: serde_json::Value = serde_json::from_str(&tool_call.function.arguments)
        .map_err(|_| CommitMessageError::InvalidToolArguments)?;
    
    let message = args.get("message")
        .and_then(|v| v.as_str())
        .ok_or(CommitMessageError::InvalidToolArguments)?;

    // Filter out issue references and merge messages
    let issue_re =
        regex::Regex::new(r"(\(?(([Ff]ix(es)?)|([Cc]loses?))?\s*#\d+\)?)|([Mm]erge [Pp].*\n)")
            .expect("Regex failed to compile");
    let commit_message = issue_re.replace_all(message, "");
    
    Ok(commit_message.trim().to_string())
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
        .ok_or_else(|| git2::Error::from_str("Could not get branch name"))?;
    let wip_branch_name = String::from("wip/") + head_branch_name;
    let head_commit = head_ref.peel_to_commit()?;
    let head_tree = head_commit.tree()?;
    let head_commit_id = head_commit.id();
    let existing_wip_branch = repo
        .find_branch(&wip_branch_name, git2::BranchType::Local)
        .or_else(|_| repo.branch(&wip_branch_name, &head_commit, true))?;
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
        info!("{}: {}", &new_commit_id.to_string()[..6], message);
    }
    Ok(wip_branch_name)
}

fn prepare_diff<'a, 'b>(
    repo: &'a Repository,
    wip_branch_name: &'b str,
) -> Result<git2::Diff<'a>, git2::Error> {
    let wip_branch = repo.find_branch(wip_branch_name, git2::BranchType::Local)?;
    let wip_tree = wip_branch.get().peel_to_tree()?;
    let mut diff_options = git2::DiffOptions::new();
    diff_options
        .minimal(true)
        .include_untracked(true)
        .context_lines(3) // default setting for diffs
        .recurse_untracked_dirs(true)
        .show_untracked_content(true);
    let diff = repo.diff_tree_to_workdir(Some(&wip_tree), Some(&mut diff_options))?;

    Ok(diff)
}

fn try_commit(
    repo: &Repository,
    wip_branch_name: &str,
    commit_message: &str,
) -> Result<git2::Oid, git2::Error> {
    // at this point, we have a wip branch ready to go. We need to add everything (other than
    // ignored stuff) in the current working directory to a tree, and commit it to the tip of the
    // wip branch.
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

fn diff_lines(diff: &git2::Diff) -> Result<Vec<String>, git2::Error> {
    let mut lines = vec![String::from("\n\n")];
    diff.print(git2::DiffFormat::Patch, |_, _, l| {
        let line = if ['+', '-', ' '].contains(&l.origin()) {
            format!(
                "{}{}",
                l.origin(),
                std::str::from_utf8(l.content()).unwrap_or("")
            )
        } else {
            std::str::from_utf8(l.content()).unwrap_or("").to_string()
        };
        lines.push(line);
        true
    })?;
    Ok(lines)
}

#[derive(Debug)]
enum ChangeHandlingError {
    Git(git2::Error),
    CommitMessage(CommitMessageError),
    Utf8(std::str::Utf8Error),
}

impl std::fmt::Display for ChangeHandlingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            ChangeHandlingError::Git(e) => write!(f, "Git Error: {}", e),
            ChangeHandlingError::CommitMessage(e) => {
                write!(f, "Error getting commit message: {}", e)
            }
            ChangeHandlingError::Utf8(e) => write!(f, "UTF-8 Error: {}", e),
        }
    }
}

impl std::error::Error for ChangeHandlingError {}

impl std::convert::From<git2::Error> for ChangeHandlingError {
    fn from(e: git2::Error) -> Self {
        ChangeHandlingError::Git(e)
    }
}

impl std::convert::From<CommitMessageError> for ChangeHandlingError {
    fn from(e: CommitMessageError) -> Self {
        ChangeHandlingError::CommitMessage(e)
    }
}

impl std::convert::From<std::str::Utf8Error> for ChangeHandlingError {
    fn from(e: std::str::Utf8Error) -> Self {
        ChangeHandlingError::Utf8(e)
    }
}

fn handle_change_inner(
    repo: &Repository,
    offset: time::UtcOffset,
) -> Result<(), ChangeHandlingError> {
    let sig = repo.signature()?;
    let name = prepare_wip_branch(repo)?;
    let diff = prepare_diff(repo, &name)?;
    let lines = diff_lines(&diff)?;
    if lines.len() <= 1 {
        debug!("Empty diff");
        return Ok(());
    }
    let text = lines.join("");
    let message = get_message(
        sig.name().unwrap_or("").to_string(),
        sig.email().unwrap_or("").to_string(),
        text,
        offset,
    )?;
    debug!("Got a commit message");
    let id = try_commit(repo, &name, &(String::from("wip: ") + &message))?;
    info!("Commit {}: {}", &id.to_string()[..6], message);
    Ok(())
}

fn handle_change(repo: &Repository, utc_offset: time::UtcOffset) {
    handle_change_inner(repo, utc_offset).unwrap_or_else(|e| error!("{}", e))
}

#[derive(Debug)]
enum AppError {
    Git(git2::Error),
    Notify(notify_debouncer_mini::notify::Error),
    Time(time::error::IndeterminateOffset),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        match self {
            AppError::Git(e) => write!(f, "Git Error: {}", e),
            AppError::Notify(e) => write!(f, "File watcher error: {}", e),
            AppError::Time(e) => write!(f, "Time error: {}", e),
        }
    }
}

impl std::error::Error for AppError {}

impl std::convert::From<git2::Error> for AppError {
    fn from(e: git2::Error) -> Self {
        AppError::Git(e)
    }
}

impl std::convert::From<notify_debouncer_mini::notify::Error> for AppError {
    fn from(e: notify_debouncer_mini::notify::Error) -> Self {
        AppError::Notify(e)
    }
}

impl std::convert::From<time::error::IndeterminateOffset> for AppError {
    fn from(e: time::error::IndeterminateOffset) -> Self {
        AppError::Time(e)
    }
}

/// Automatic work-in-progress commits with descriptive commit messages generated by GPT-3 Codex
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// How long to wait to accumulate changes before committing, in secs. Recommended to be >= 0.1
    #[arg(short, long, default_value_t = 0.1)]
    time_delay: f64,
}

fn main() -> Result<(), AppError> {
    let args = Args::parse();

    let offset = time::UtcOffset::current_local_offset()?;
    use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
    use tracing_subscriber::fmt::time::OffsetTime;
    let format = tracing_subscriber::fmt::format()
        .with_ansi(false)
        .with_level(false)
        .with_target(false)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_timer(OffsetTime::new(
            offset,
            format_description!("[hour]:[minute]:[second]"),
        ));
    tracing_subscriber::fmt().event_format(format).init();
    let repository = Repository::discover(".")?;
    let path = repository
        .path()
        .parent()
        .expect("Git repository does not appear to have a parent dir")
        .to_path_buf();
    debug!("Found git repository at {}", path.display());

    debug!("Doing an unconditional first pass in case there are existing changes to commit.");
    handle_change(&repository, offset);

    let mut debouncer = new_debouncer(
        std::time::Duration::from_secs_f64(args.time_delay),
        None,
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                debug!("{} file events", events.len());
                let any_non_git_files = events.iter().any(|e| {
                    let p = &e.path;
                    !p.components().any(|part| {
                        part == std::path::Component::Normal(std::ffi::OsStr::new(".git"))
                    })
                });
                if any_non_git_files {
                    debug!("Found files not in a .git directory");
                    handle_change(&repository, offset);
                } else {
                    debug!("No files outside of .git changed");
                }
            }
            Err(e) => error!("Error watching files: {:?}", e),
        },
    )?;

    debouncer.watcher().watch(&path, RecursiveMode::Recursive)?;
    debug!("Set up filewatcher");

    loop {
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
}
