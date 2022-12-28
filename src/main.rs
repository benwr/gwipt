use std::path::Path;

use git2::{Diff, DiffFormat, Repository,};

use notify_debouncer_mini::{DebounceEventResult, DebouncedEvent, new_debouncer, notify::{RecursiveMode, Result}};

fn try_commit(r: &Repository) {
    // 1. What branch are we on?
    let head_name = if let Ok(head_ref) = r.head() {
        if let Some(target) = head_ref.symbolic_target() {
            target.to_string()
        } else {
            eprintln!("Either repository is empty, or is a bare repository, or is in a detached-head state.");
            eprintln!("gwipt requires that you check out a non-empty branch in a non-bare repository.");
            return
        }
    } else {
        eprintln!("Either repository is empty, or is a bare repository, or is in a detached-head state.");
        eprintln!("gwipt requires that you check out a non-empty branch in a non-bare repository.");
        return
    };

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
