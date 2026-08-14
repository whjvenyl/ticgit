use std::io::{self, BufRead, Write};

use anyhow::Result;
use clap::Parser;

use crate::commands::{open_store, SessionGitDir};
use crate::session_state::State;

#[derive(Debug, Parser)]
pub struct Args {
    /// Skip the confirmation prompt. Use with care: this deletes every
    /// ticket, writeup, view, and user mapping in the store.
    #[arg(long = "yes", short = 'y')]
    pub yes: bool,
}

pub fn run(args: Args) -> Result<()> {
    let store = open_store()?;

    let count = store.list()?.len();
    if count == 0 {
        println!("Store is already empty — nothing to reset.");
        return Ok(());
    }

    if !args.yes {
        println!(
            "This will permanently delete all {count} ticket(s) and every writeup, view, \
             and user mapping in this repository's ticgit store."
        );
        print!("Type 'yes' to confirm: ");
        io::stdout().flush()?;
        let stdin = io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        if line.trim() != "yes" {
            println!("Reset cancelled.");
            return Ok(());
        }
    }

    store.reset()?;

    // Clear the checked-out ticket from session state so stale IDs
    // don't linger after the wipe.
    let git_dir = store.session().repo_git_dir();
    let mut state = State::load().unwrap_or_default();
    state.clear_current(&git_dir);
    state.save()?;

    println!("Reset complete — ticgit store is now empty.");
    Ok(())
}
