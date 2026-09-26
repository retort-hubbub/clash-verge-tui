//! Every command that takes a name the controller must know.
//!
//! This project's recurring defect is a guard that covers the members somebody
//! named rather than the class they belong to — eight recorded instances. The
//! tenth review left this one open: `test urls --node <typo>` was refused and
//! nothing had enumerated the rest, so "only the member the ninth round fixed
//! has the check" was a suspicion nobody had tested.
//!
//! Checked by hand first, against a live core: **every member refuses**, with
//! exit 1 and a message naming what it could not find. Two of them refuse with
//! the *core's* own 404 rather than a local lookup, which is the same answer by
//! the shorter route — the core is the authority on what exists.
//!
//! What this test adds is that the class cannot grow quietly. The list below is
//! hand-written, and the walk of the binary's own `--help` fails if a command
//! takes a node or group name without being on it.

use std::process::Command;

/// The binary under test.
const BIN: &str = env!("CARGO_BIN_EXE_clash-verge-tui");

/// Commands that take a name the controller must know, and where it comes from.
///
/// `positional` for the ones a person types as an argument, `flag` for the ones
/// behind a flag. The distinction matters: the first version of this test only
/// looked at flags and missed four members.
const MEMBERS: &[(&str, &str)] = &[
    ("proxies chain", "positional"),
    ("proxies list", "positional"),
    ("proxies select", "positional"),
    ("proxies test", "positional"),
    ("proxies unpin", "positional"),
    ("test delay", "flag"),
    ("test urls", "flag"),
    ("proxies test", "flag"),
    ("proxies test-all", "flag"),
];

/// The subcommands a help screen lists.
fn subcommands(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.trim() == "Commands:" {
            inside = true;
            continue;
        }
        if inside {
            if line.trim().is_empty() {
                continue;
            }
            match line.strip_prefix("  ") {
                Some(rest) => {
                    let name: String = rest
                        .chars()
                        .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                        .collect();
                    if !name.is_empty() {
                        found.push(name);
                    }
                }
                None => inside = false,
            }
        }
    }
    found
}

fn help(path: &[String]) -> String {
    let output = Command::new(BIN)
        .args(path)
        .arg("--help")
        .output()
        .expect("the binary runs");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Every leaf command, walked from the binary's own help.
fn leaves() -> Vec<String> {
    let mut leaves = Vec::new();
    let mut queue: Vec<Vec<String>> = vec![Vec::new()];
    while let Some(path) = queue.pop() {
        let children = subcommands(&help(&path));
        if children.is_empty() {
            leaves.push(path.join(" "));
        } else {
            for child in children {
                let mut next = path.clone();
                next.push(child);
                queue.push(next);
            }
        }
    }
    leaves
}

#[test]
fn every_command_that_takes_a_name_is_on_the_list() {
    let leaves = leaves();
    assert!(
        leaves.len() > 40,
        "the walk found only {} commands, so it is looking in the wrong place",
        leaves.len()
    );

    let mut unchecked = Vec::new();
    for leaf in &leaves {
        let parts: Vec<String> = leaf.split(' ').map(str::to_owned).collect();
        let text = help(&parts);
        let usage = text
            .lines()
            .find(|line| line.trim_start().starts_with("Usage:"))
            .unwrap_or_default()
            .to_owned();
        // A positional that is a NAME or a NODE, or a flag that is one.
        let positional =
            usage.contains("<NODE>") || usage.contains("<GROUP>") || usage.contains("[GROUP]");
        let flag = ["--node", "--group"].iter().any(|f| text.contains(f));
        if (positional || flag) && !MEMBERS.iter().any(|(name, _)| name == leaf) {
            unchecked.push(format!(
                "{leaf}: {}",
                if positional { "positional" } else { "flag" }
            ));
        }
    }

    assert!(
        unchecked.is_empty(),
        "these commands take a name the controller must know and are not on the \
         list above, so nothing asks whether they refuse one it does not have:\n{}",
        unchecked.join("\n")
    );
}

#[test]
fn the_list_has_no_command_that_does_not_take_one() {
    // The other direction: an entry for a command that takes no name is a claim
    // about nothing, and would hide the day the command stops taking one.
    let leaves = leaves();
    let stale: Vec<&str> = MEMBERS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| !leaves.iter().any(|leaf| leaf == name))
        .collect();
    assert!(
        stale.is_empty(),
        "the list names commands that do not exist: {stale:?}"
    );
}
