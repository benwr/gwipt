use std::path::Path;

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
    let headoid = headcommit.id();

    // Does the wip branch already exist? If so, is HEAD reachable from it? If not, create it and
    // point it at HEAD. otherwise, leave it be.

    // Make the wip branch if it doesn't exist
    let (wipbranch, wipcommit) = if let Ok(branch) = r.find_branch(&wipbranchname, git2::BranchType::Local) {
        if let Ok(commit) = branch.get().peel_to_commit() {
            (branch, commit)
        } else {
            (r.branch(&wipbranchname, &headcommit, true).unwrap(), headcommit)
        }
    } else {
        (r.branch(&wipbranchname, &headcommit, true).unwrap(), headcommit)
    };

    let wipoid = wipcommit.id();

    if let Ok(true) = (r.graph_descendant_of(wipoid, headoid).map(|desc| desc || (headoid == wipoid))) {
        eprintln!("Is an ancestor");
    } else {
        eprintln!("Not an ancestor");
    }

    // 2. Is there a diff?
    eprintln!("");
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
