//! Questions for a human at a terminal. Every verb that asks does all its
//! asking before it does anything, so an unanswered question never leaves a
//! guest half made. Without a terminal there are no questions: the caller
//! fails with the `config set` command that would have answered.

use std::io::{self, BufRead, Write};

use anyhow::{bail, Context, Result};

/// Whether stdin is a terminal a person can answer on.
pub fn interactive() -> bool {
    // SAFETY: isatty reads a file descriptor's attributes and nothing else.
    unsafe { libc::isatty(0) == 1 }
}

fn read_line() -> Result<String> {
    let mut line = String::new();
    io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the answer")?;
    if line.is_empty() {
        bail!("no answer (end of input)");
    }
    Ok(line.trim().to_string())
}

/// One choice from a numbered list. `describe` renders each item's line.
pub fn choose<T>(question: &str, items: &[T], describe: impl Fn(&T) -> String) -> Result<usize> {
    if items.is_empty() {
        bail!("nothing to choose from");
    }
    eprintln!("{question}");
    for (i, it) in items.iter().enumerate() {
        eprintln!("  {:>2}) {}", i + 1, describe(it));
    }
    loop {
        eprint!("> ");
        io::stderr().flush().ok();
        let a = read_line()?;
        match a.parse::<usize>() {
            Ok(n) if (1..=items.len()).contains(&n) => return Ok(n - 1),
            _ => eprintln!("a number from 1 to {}", items.len()),
        }
    }
}

/// A yes/no question; `default` is the answer to an empty line.
pub fn yes_no(question: &str, default: bool) -> Result<bool> {
    eprint!("{question} [{}] ", if default { "Y/n" } else { "y/N" });
    io::stderr().flush().ok();
    let a = read_line()?;
    Ok(match a.to_ascii_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    })
}
