//! The REPL's spreadsheet verbs must *answer* when there is no spreadsheet.
//!
//! corro's own build never creates a `Spreadsheet` node, and the model the
//! zork REPL starts with is the one corro builds. So `grid` and `sheet` are
//! typed into exactly the situation this file is about: a model that holds
//! widgets, and no sheet to aim at.
//!
//! `grid` used to `return` on an unresolved target without printing, so on
//! that model it produced no output at all. Silence is the worst of the three
//! answers a reader can imagine -- it cannot be read as an empty sheet, an
//! error, or a verb that does not exist. `sheet` had its own copy of the
//! resolution logic and did explain itself, so the two verbs disagreed about
//! the same model.
//!
//! # Why this runs a binary rather than calling the REPL directly
//!
//! The bug was never in *deciding* to say something; it was in the arm that
//! threw the decision away. `execute_command` prints to stdout, which a unit
//! test cannot intercept, so a test written against the resolver passes
//! happily while the verb stays silent. That was checked: with the silent
//! `Err(_) => return` restored, every resolver-level test still passed. Only
//! running the real program and reading what it printed pins the behaviour.
//!
//! The fixture is `zork-repl`, a window with a label and an entry and no
//! `Spreadsheet`, handed straight to the REPL. `zork-demo` builds the same kind
//! of tree but its `app.run()` never takes over the event loop here, and
//! `zork-sheet` does enter the REPL but builds a sheet -- so neither could ask
//! this question.
//!
//! # What this writes
//!
//! Nothing. The example is run with one command on stdin and its stdout is
//! read; no file is created, and no report is regenerated.

#![cfg(feature = "zork")]

use std::io::Write as _;
use std::process::{Command, Stdio};

/// Where `cargo` put the built examples, next to this test binary.
///
/// The build dir is per-crate here (`target/<profile>/build/<crate>-<hash>/out`)
/// rather than the usual `deps`, so the examples are found by walking up to
/// the profile directory instead of by assuming a layout.
fn example_path(name: &str) -> std::path::PathBuf {
    let p = std::env::current_exe().expect("the test binary has a path");
    for dir in p.ancestors() {
        let candidate = dir.join("examples").join(name);
        if candidate.is_file() {
            return candidate;
        }
        // Only walk as far as the profile directory; anything above it is not
        // ours to search.
        if dir.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n == "debug" || n == "release"
        }) {
            break;
        }
    }
    panic!("could not find the {name} example; was it built? (cargo test builds it)");
}

/// Run the demo with `command` typed at it and return everything it printed.
///
/// The demo hands off to the REPL after building its widgets, so a command on
/// stdin is dispatched by the real command loop over the real model.
fn ask(command: &str) -> String {
    let exe = example_path(if cfg!(windows) { "zork-repl.exe" } else { "zork-repl" });
    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("could not run {}: {e}", exe.display()));
    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(format!("{command}\nquit\n").as_bytes())
        .expect("the demo reads stdin");
    let out = child.wait_with_output().expect("the demo terminates on `quit`");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// The verb's own words, with the REPL's furniture removed.
///
/// The REPL prints a room description before and after every command, so "did
/// it print anything" is not a question with a useful answer -- the answer is
/// yes either way. What has to be checked is that the verb said something
/// *about the spreadsheet*, so the banner the fixture prints and the room
/// description are dropped and the verb's own line is what is left. A verb
/// that says nothing yields an empty string.
fn reply(command: &str) -> String {
    ask(command)
        .lines()
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && *l != ">"
                && !l.contains("dark room")
                // The fixture's own two lines, printed before the REPL starts.
                && !l.starts_with("No spreadsheet here")
                && !l.starts_with("Type 'grid'")
                && *l != "Goodbye!"
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An untargeted spreadsheet verb on a model with no spreadsheet must say the
/// sheet is absent.
///
/// This is the case that was silent. `sheet` already answered; `grid` did not,
/// and the two verbs were asked the same question of the same model.
#[test]
fn an_untargeted_spreadsheet_verb_says_the_sheet_is_absent() {
    const ABSENT: &str = "There is no Spreadsheet in this model.";
    for verb in ["grid", "sheet"] {
        let said = reply(verb);
        assert!(
            said.contains(ABSENT),
            "`{verb}` on a model with no spreadsheet said:\n{said:?}\n\
             expected it to say {ABSENT:?}. Silence cannot be read as an empty \
             sheet, an error, or a missing verb."
        );
    }
}

/// The two verbs must answer the same question the same way.
///
/// Not a style rule: they resolve their target through one function, and the
/// bug was that `grid` discarded what that function returned. If the answers
/// diverge, one of them is discarding again.
#[test]
fn both_spreadsheet_verbs_answer_alike() {
    assert_eq!(
        reply("grid"),
        reply("sheet"),
        "`grid` and `sheet` were asked the same question of the same model"
    );
}

/// A named target gets a *different* answer, and must still get one.
///
/// `grid 1` on this model names the Window: something that exists, and is not
/// a sheet. The two tempting wrong answers are "the sheet is empty" -- which
/// every cell read on a non-spreadsheet invites, since it has no cells -- and
/// "there is no spreadsheet", which blames the model for a target the reader
/// chose. Both were live; the second is what `grid 1` printed before.
#[test]
fn a_target_that_is_not_a_spreadsheet_is_named_as_such() {
    const WRONG_KIND: &str = "That is not a Spreadsheet.";
    let said = reply("grid 1");
    assert!(
        said.contains(WRONG_KIND),
        "`grid 1` on a model with no spreadsheet said:\n{said:?}\n\
         expected {WRONG_KIND:?} -- the target exists, it is just not a sheet."
    );
    assert!(
        !said.contains("sheet is empty"),
        "`grid 1` reported an empty sheet for a model that has no sheet at all:\n{said:?}"
    );
}

/// The verbs must not be silent on *any* spelling, however it is aimed.
///
/// A verb that answers one spelling and swallows another is the same defect in
/// a narrower window: the reader has typed something reasonable and been told
/// nothing. Each of these names a target the demo's tree can be asked about.
#[test]
fn no_spelling_of_a_spreadsheet_verb_is_silent() {
    for verb in ["grid", "grid 1", "grid north", "sheet", "sheet 1", "sheet show"] {
        let said = reply(verb);
        assert!(
            !said.is_empty(),
            "`{verb}` printed nothing at all; a reader who types it learns nothing \
             about either the model or the verb"
        );
    }
}
