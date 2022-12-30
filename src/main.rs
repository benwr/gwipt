use std::path::Path;
use std::str;

use chrono::{Local, DateTime, TimeZone};
use git2::{Diff, DiffFormat, Repository,};
use regex::Regex;
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

use notify_debouncer_mini::{DebounceEventResult, DebouncedEvent, new_debouncer, notify::{RecursiveMode, Result}};

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
    index: usize,
    finish_reason: String,
}

#[derive(Debug, Deserialize)]
struct Usage {
    completion_tokens: usize,
    total_tokens: usize,
}
#[derive(Debug, Deserialize)]
struct OpenAiResponse {
    id: String,
    object: String,
    created: usize,
    model: String,
    choices: Vec<ResponseChoice>,
    usage: Usage,
}

fn get_commit_message(name: String, email: String, diff: String) -> String {
    let now = Local::now();
    let prefix = format!("Author: {} <{}>\nDate:   {}", name, email, now.format("%a %b %-d %H:%M:%S %Y %z"));
    let key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY environment variable must be set");
    let client = Client::new();
    let prefixlen = prefix.len();
    let request = OpenAiRequest {
        model: "text-davinci-003".to_string(),
        prompt: prefix,
        suffix: diff.chars().take((2048 - 100) * 2 - prefixlen).collect(),
        temperature: 0.7,
        max_tokens: 100,
        top_p: 0.9,
        frequency_penalty: 0.1,
        presence_penalty: 0.0,
    };
    // TODO limit length of diff to ensure no errors
    let response = client.post("https://api.openai.com/v1/completions")
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", key))
        .json(&request)
        .send()
        .unwrap();
    let response: OpenAiResponse = response.json().unwrap();

    let issue_re = Regex::new(r"(\(?(([Ff]ix(es)?)|([Cc]loses?))?\s*#\d+\)?)|([Mm]erge [Pp].*\n)").unwrap();
    let commit_message = issue_re.replace_all(&response.choices[0].text, "");
    commit_message.trim().split('\n').next().unwrap().to_string()
}

fn try_commit(r: &Repository) {
    // 1. What branch are we on?
    let (headbranchname, headcommit) = if let Ok(head_ref) = r.head() {
        if !head_ref.is_branch() {
            eprintln!("Either the repository is empty, or is a bare repository, or is in a detached-head state.");
            eprintln!("gwipt requires that you check out a non-empty branch.");
            return
        }
        if let (Some(target), Ok(commit)) = (head_ref.shorthand(), head_ref.peel_to_commit()) {
            (target.to_string(), commit)
        } else {
            eprintln!("Either the repository is empty, or is a bare repository, or is in a detached-head state.");
            eprintln!("gwipt requires that you check out a non-empty branch.");
            return
        }
    } else {
        eprintln!("Either the repository is empty, or is a bare repository, or is in a detached-head state.");
        eprintln!("gwipt requires that you check out a non-empty branch.");
        return
    };

    let wipbranchname = String::from("wip/") + &headbranchname;
    let headid = headcommit.id();

    // Does the wip branch already exist? If so, is HEAD reachable from it? If not, create it and
    // point it at HEAD. otherwise, leave it be.

    // Make the wip branch if it doesn't exist
    let (wipbranch, wipcommitid) = if let Ok(branch) = r.find_branch(&wipbranchname, git2::BranchType::Local) {
        eprintln!("found branch");
        if let Ok(commit) = branch.get().peel_to_commit() {
            eprintln!("peeled branch to commit {}", commit.id());
            (branch, commit.id())
        } else {
            eprintln!("could not peel branch to commit");
            (r.branch(&wipbranchname, &headcommit, true).unwrap(), headid)
        }
    } else {
        eprintln!("could not find branch");
        (r.branch(&wipbranchname, &headcommit, true).unwrap(), headid)
    };

    let wipbranch = if let Ok(true) = (r.graph_descendant_of(wipcommitid, headid).map(|desc| desc || (headid == wipcommitid))) {
        // Is an ancestor; we just want to make a new commit on the same branch
        eprintln!("wipbranch := wipbranch");
        wipbranch
    } else {
        // Not an ancestor; we want to reset the branch to point at the current commit
        eprintln!("wipbranch := new branch from head");
        r.branch(&wipbranchname, &headcommit, true).unwrap()
    };

    // 2. Is there a diff?
    let mut diff_options = git2::DiffOptions::new();
    diff_options.minimal(true);
    let mut diff_lines = vec![String::from("\n\n")];
    let diff = r.diff_tree_to_workdir(Some(&wipbranch.get().peel_to_tree().unwrap()), Some(&mut diff_options)).unwrap();
    diff.print(git2::DiffFormat::Patch, |_, _, l| {
        let line = if ['+', '-', ' '].contains(&l.origin()) {
            format!("{}{}", l.origin(), str::from_utf8(l.content()).unwrap())
        } else {
            format!("{}", str::from_utf8(l.content()).unwrap())
        };
        diff_lines.push(line);
        true
    }).unwrap();
    if diff_lines.len() <= 1 {
        return
    }

    // 3. Ask GPT-3 for commit message based on the diff
    let config = r.config().unwrap();
    let name = config.get_string("user.name").unwrap();
    let email = config.get_string("user.email").unwrap();
    let difftext = format!("{}", diff_lines.join(""));
    let commit_message = "wip: ".to_string() + &get_commit_message(name, email, difftext);

    // 4. Commit to wip/branch
    let me = r.signature().unwrap();
    let wipcommit = r.find_commit(wipcommitid).unwrap();
    let mut wipindex = r.apply_to_tree(&wipcommit.tree().unwrap(), &diff, None).unwrap();
    let wiptreeid = wipindex.write_tree_to(&r).unwrap();
    let wiptree = r.find_tree(wiptreeid).unwrap();


    let commit_id = r.commit(None, &me, &me, &commit_message, &wiptree, &[&wipcommit]).unwrap();
    r.branch(&wipbranchname, &r.find_commit(commit_id).unwrap(), true).unwrap();
    println!("Committed to {}: {} {}", &wipbranchname, &commit_id, &commit_message);
}

fn main() -> Result<()> {
    // TODO replace unwrap with logging and error codes
    let repository = Repository::discover(".").unwrap();
    let cwd = std::env::current_dir().unwrap();

    let mut debouncer = new_debouncer(
        std::time::Duration::new(0, 100_000_000),
        None,
        move |res: DebounceEventResult| {
            match res {
                Ok(events) => {
                    let any_non_git_files =  events.iter().any(|e| {
                        let p = &e.path;
                        !p.components().any(|part| part == std::path::Component::Normal(std::ffi::OsStr::new(".git")))
                    });
                    if any_non_git_files {
                        try_commit(&repository);
                    }
                }
                Err(e) => eprintln!("Error watching files: {:?}", e),
            }
        }).unwrap();

    debouncer.watcher().watch(Path::new("."), RecursiveMode::Recursive)?;

    loop {}

    Ok(())
}
