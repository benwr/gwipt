use std::path::Path;
use std::str;

use chrono::{Local, DateTime, TimeZone};
use git2::{Diff, DiffFormat, Repository,};

use notify_debouncer_mini::{DebounceEventResult, DebouncedEvent, new_debouncer, notify::{RecursiveMode, Result}};

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
        if let Ok(commit) = branch.get().peel_to_commit() {
            (branch, commit.id())
        } else {
            (r.branch(&wipbranchname, &headcommit, true).unwrap(), headid)
        }
    } else {
        (r.branch(&wipbranchname, &headcommit, true).unwrap(), headid)
    };

    let wipbranch = if let Ok(true) = (r.graph_descendant_of(wipcommitid, headid).map(|desc| desc || (headid == wipcommitid))) {
        // Is an ancestor; we just want to make a new commit on the same branch
        wipbranch
    } else {
        // Not an ancestor; we want to reset the branch to point at the current commit
        r.branch(&wipbranchname, &headcommit, true).unwrap()
    };

    // 2. Is there a diff?
    let mut diff_options = git2::DiffOptions::new();
    diff_options.include_untracked(true).recurse_untracked_dirs(true);
    let mut diff_lines = vec![String::from("\n\n")];
    if let Ok(diff) = r.diff_tree_to_workdir(Some(&wipbranch.into_reference().peel_to_tree().unwrap()), Some(&mut diff_options)) {
        diff.print(git2::DiffFormat::Patch, |_, _, l| {
            let line = if ['+', '-', ' '].contains(&l.origin()) {
                format!("{}{}", l.origin(), str::from_utf8(l.content()).unwrap())
            } else {
                format!("{}", str::from_utf8(l.content()).unwrap())
            };
            diff_lines.push(line);
            true
        }).unwrap();
    }
    let config = r.config().unwrap();
    let name = config.get_string("user.name").unwrap();
    let email = config.get_string("user.email").unwrap();
    let now = Local::now();
    println!("Author: {} <{}>", name, email);
    println!("Date:   {}", now.format("%a %b %-d %H:%M:%S %Y %z"));
    print!("\n\n");
    println!("COMMIT MESSAGE HERE");
    print!("{}", diff_lines.join(""));

    //let diff = r.diff_tree_to_workdir().unwrap();

    // 3. Ask GPT-3 for commit message based on the diff
    // 4. Commit to wip/branch
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
                        !(p.starts_with(cwd.join(".git/")) || p.starts_with(".git/") || p.starts_with("./.git/"))
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
