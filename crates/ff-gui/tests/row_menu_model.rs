//! The row context menu's plumbing, with no display and no toolkit.
//!
//! `tests/row_menu_gtk.rs` and `tests/row_menu_row_gtk.rs` check the GTK menu
//! itself, and `tests/row_menu_row_mac.rs` the AppKit one; both need a window
//! server, so on a headless `cargo test` they either return early or do not run
//! at all. The three steps between a right-click and a launched program are
//! platform-free, though — the hit test, the item list and the dispatch — and
//! they are what this file pins, so a plain `cargo test` exercises them on every
//! platform:
//!
//! * [`context_menu_target`]: which row a right-click acts on, and that a left
//!   click, a click past the rows, or a click with no row opens nothing (so a
//!   stale selection can never be the target);
//! * [`row_menu_model`]: the menu's items and their enabled state — every item
//!   enabled over a row, every item disabled without one;
//! * [`run_row_action_with`]: every item runs its **own** command, the clipboard
//!   items put the clicked row's path (or its directory) on the clipboard and
//!   report it, a failing clipboard is reported rather than swallowed, and no
//!   two launch items dispatch the same command.

use std::cell::RefCell;

use ff_gui::{
    containing_dir, context_menu_target, row_menu_model, run_row_action_with, streaming_rows,
    RowAction,
};

const PATH: &str = "/tmp/ff-gui-cm/typing-alpha.txt";
const DIR: &str = "/tmp/ff-gui-cm";

fn rows() -> Vec<String> {
    vec![
        "/tmp/ff-gui-cm/one.rs".to_string(),
        "/tmp/ff-gui-cm/two.rs".to_string(),
    ]
}

/// A right-click acts on the row under the pointer and on nothing else.
#[test]
fn right_click_picks_the_row_under_the_pointer() {
    let rows = rows();
    assert_eq!(
        context_menu_target(3, Some(0), &rows).as_deref(),
        Some("/tmp/ff-gui-cm/one.rs"),
        "row 0 did not resolve to its own path"
    );
    assert_eq!(
        context_menu_target(3, Some(1), &rows).as_deref(),
        Some("/tmp/ff-gui-cm/two.rs"),
        "row 1 did not resolve to its own path"
    );
    assert_eq!(
        context_menu_target(3, None, &rows),
        None,
        "a right-click with no row under it opened a menu, so it would act on a stale selection"
    );
    assert_eq!(
        context_menu_target(3, Some(2), &rows),
        None,
        "a row index past the rows fell back to the last row instead of opening nothing"
    );
    assert_eq!(
        context_menu_target(3, Some(-1), &rows),
        None,
        "a negative row opened a menu"
    );
    assert_eq!(
        context_menu_target(3, Some(0), &[]),
        None,
        "a right-click on an empty list opened a menu"
    );
    assert_eq!(
        context_menu_target(1, Some(0), &rows),
        None,
        "a left click opened a row menu instead of starting a drag"
    );
    assert_eq!(
        context_menu_target(2, Some(0), &rows),
        None,
        "a middle click opened a row menu"
    );
}

/// Over a row the menu offers every action, in the app's order, all enabled.
#[test]
fn menu_over_a_row_offers_every_action_enabled() {
    let rows = rows();
    let target = context_menu_target(3, Some(1), &rows).expect("row 1 has no menu target");
    let items = row_menu_model(Some(&target));

    let labels: Vec<&str> = items.iter().map(|i| i.label).collect();
    // The item set is pinned here, item by item: the four the requirement names
    // plus `Copy folder path` and `Open with…`. `RowAction::all`'s doc records
    // why the superset is kept; this assertion is what makes that decision
    // durable, so dropping, adding or renaming an item fails the suite.
    assert_eq!(
        labels,
        vec![
            "Copy full path",
            "Copy folder path",
            "Open",
            "Open with…",
            "Open containing folder",
            "Open in terminal",
        ],
        "the row menu's item set changed; the four required actions must all be present: {items:?}"
    );
    let expected: Vec<&str> = RowAction::all().iter().map(|a| a.label()).collect();
    assert_eq!(
        labels, expected,
        "the menu is not the app's action list, in the app's order: {items:?}"
    );
    assert!(
        items.iter().all(|i| i.enabled),
        "an item is disabled although the pointer is on a row: {items:?}"
    );
    for named in [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ] {
        let item = items
            .iter()
            .find(|i| i.action == named)
            .unwrap_or_else(|| panic!("the menu does not offer {:?}", named.label()));
        assert!(item.enabled, "{:?} is disabled over a row", named.label());
        assert_eq!(item.label, named.label());
    }
    assert_eq!(
        items.len(),
        RowAction::all().len(),
        "the menu holds {} items, not the app's {}",
        items.len(),
        RowAction::all().len()
    );
}

/// With no row under the pointer every item is disabled, and the app opens no
/// menu at all (the target is `None`).
#[test]
fn menu_without_a_row_disables_every_item() {
    let rows = rows();
    let target = context_menu_target(3, None, &rows);
    assert!(target.is_none(), "a click with no row produced a target");
    let items = row_menu_model(target.as_deref());
    let expected: Vec<&str> = RowAction::all().iter().map(|a| a.label()).collect();
    assert_eq!(
        items.iter().map(|i| i.label).collect::<Vec<_>>(),
        expected,
        "the no-row menu is a different item list: {items:?}"
    );
    assert!(
        items.iter().all(|i| !i.enabled),
        "an item is enabled although no row is under the pointer: {items:?}"
    );
}

/// The as-you-type (streaming) list feeds the same row menu as the settled one:
/// its rows come from [`streaming_rows`], and the row under the pointer resolves
/// inside the rows painted so far — not against the settled paths, whose indices
/// would name a different file.
#[test]
fn streaming_rows_share_the_settled_lists_row_menu() {
    let hits: Vec<String> = vec![
        "/tmp/ff-gui-stream/first.rs".to_string(),
        "/tmp/ff-gui-stream/second.rs".to_string(),
        "/tmp/ff-gui-stream/third.rs".to_string(),
    ];

    // A mid-stream tick where the daemon has delivered every hit but only the
    // first page is painted: the filter keeps all three, the paint shows two.
    let (kept, shown) = streaming_rows(&hits, None, 2);
    assert_eq!(kept, hits, "the streaming filter dropped a hit the list keeps");
    assert_eq!(
        shown,
        hits[..2].to_vec(),
        "the streaming paint is not the first page of the kept hits"
    );

    // A tick whose painted rows do not start at the settled list's first row —
    // the case a stale mapping gets wrong.
    let (kept, shown) = streaming_rows(&hits[1..], None, 8);
    assert_eq!(kept, hits[1..].to_vec());
    assert_eq!(shown, hits[1..].to_vec());

    // Row 0 of that paint is `hits[1]`; resolving against the settled list would
    // hand back `hits[0]`.
    let target = context_menu_target(3, Some(0), &shown)
        .expect("a right-click on the painted row 0 resolved to nothing");
    assert_eq!(
        target, hits[1],
        "a right-click on streaming row 0 did not resolve the row painted there"
    );

    let items = row_menu_model(Some(&target));
    assert!(
        items.iter().all(|i| i.enabled),
        "a streaming row's menu has a disabled item: {items:?}"
    );
    for named in [
        RowAction::Open,
        RowAction::OpenContainingFolder,
        RowAction::CopyPath,
        RowAction::OpenInTerminal,
    ] {
        let item = items
            .iter()
            .find(|i| i.action == named)
            .unwrap_or_else(|| panic!("a streaming row's menu lacks {:?}", named.label()));
        assert!(
            item.enabled,
            "{:?} is disabled on a streaming row",
            named.label()
        );
    }

    let copied: RefCell<Option<String>> = RefCell::new(None);
    let status = run_row_action_with(
        RowAction::CopyPath,
        &target,
        |text| {
            *copied.borrow_mut() = Some(text.to_string());
            Ok(())
        },
        |program, args| panic!("Copy path launched {program} {args:?} instead of copying"),
    );
    assert_eq!(
        *copied.borrow(),
        Some(hits[1].clone()),
        "Copy path on a streaming row put the wrong path on the clipboard"
    );
    assert_eq!(status, format!("copied {}", hits[1]));

    // Clicking past the painted rows opens nothing mid-stream either, and a
    // later tick that paints more rows re-resolves the same row index.
    assert_eq!(context_menu_target(3, Some(2), &shown), None);
    let (_, grown) = streaming_rows(&hits, None, 8);
    assert_eq!(
        context_menu_target(3, Some(0), &grown).as_deref(),
        Some(hits[0].as_str()),
        "after the paint grew, row 0 still resolved to the earlier tick's row"
    );
}

/// Copy path puts the clicked row's path on the clipboard — not the folder, not
/// a stale row — and Copy folder path puts its directory there.
#[test]
fn copy_actions_put_the_clicked_rows_path_on_the_clipboard() {
    let copied: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let status = run_row_action_with(
        RowAction::CopyPath,
        PATH,
        |text| {
            copied.borrow_mut().push(text.to_string());
            Ok(())
        },
        |program, args| panic!("Copy path launched {program} {args:?} instead of copying"),
    );
    assert_eq!(
        *copied.borrow(),
        vec![PATH.to_string()],
        "Copy path did not reach the clipboard with the row's path"
    );
    assert_eq!(
        status,
        format!("copied {PATH}"),
        "Copy path reported {status:?}"
    );

    let copied: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let status = run_row_action_with(
        RowAction::CopyFolderPath,
        PATH,
        |text| {
            copied.borrow_mut().push(text.to_string());
            Ok(())
        },
        |program, args| panic!("Copy folder path launched {program} {args:?}"),
    );
    assert_eq!(
        *copied.borrow(),
        vec![DIR.to_string()],
        "Copy folder path copied something other than the row's directory"
    );
    assert_eq!(status, format!("copied {DIR}"));

    // A clipboard that refuses must be reported, not silently swallowed into a
    // success line: the status line is the only feedback a copy gets.
    let status = run_row_action_with(
        RowAction::CopyPath,
        PATH,
        |_| Err("the clipboard is closed".to_string()),
        |program, args| panic!("Copy path launched {program} {args:?}"),
    );
    assert!(
        status.starts_with("copy failed: ") && status.contains("the clipboard is closed"),
        "a failed copy reported {status:?}"
    );
}

/// Every launching item runs its own command for the clicked row: the file for
/// Open / Open with, the containing directory for the folder and terminal
/// actions, and no two items share a command (so an implementation that wired
/// them all to one action would fail).
#[test]
fn every_launching_action_dispatches_its_own_command() {
    let mut seen: Vec<(RowAction, String, Vec<String>)> = Vec::new();
    for action in RowAction::all() {
        if action.is_clipboard() {
            continue;
        }
        let recorded: RefCell<Option<(String, Vec<String>)>> = RefCell::new(None);
        let status = run_row_action_with(
            action,
            PATH,
            |text| panic!("{action:?} launched but copied {text:?} to the clipboard"),
            |program, args| {
                *recorded.borrow_mut() = Some((program.to_string(), args.to_vec()));
                Ok(())
            },
        );
        let (program, args) = recorded
            .into_inner()
            .unwrap_or_else(|| panic!("{action:?} dispatched no command"));
        assert!(
            !program.is_empty(),
            "{action:?} dispatched an empty program name"
        );
        assert!(
            status.starts_with(action.label()),
            "{action:?} reported {status:?}, not its own label"
        );
        assert!(
            status.contains(&program),
            "{action:?} reported {status:?}, which names no program"
        );
        let joined = args.join(" ");
        match action {
            RowAction::Open | RowAction::OpenWith => assert!(
                args.iter().any(|a| a == PATH || a.ends_with(PATH)),
                "{action:?} ran {program} {args:?}, which does not name the row's file"
            ),
            RowAction::OpenContainingFolder => assert!(
                args.iter()
                    .any(|a| a == DIR || a == PATH || a.ends_with(PATH) || a.ends_with(DIR)),
                "{action:?} ran {program} {args:?}, which names neither the file nor its directory"
            ),
            RowAction::OpenInTerminal => assert!(
                joined.contains(DIR),
                "{action:?} ran {program} {args:?}, which does not open the row's directory {DIR:?}"
            ),
            RowAction::CopyPath | RowAction::CopyFolderPath => {
                unreachable!("clipboard actions are skipped above")
            }
        }
        seen.push((action, program, args));
    }
    assert_eq!(
        seen.len(),
        RowAction::all().len() - 2,
        "not every launching action was exercised: {seen:?}"
    );
    for (i, (action, program, args)) in seen.iter().enumerate() {
        for (other, other_program, other_args) in &seen[i + 1..] {
            assert!(
                program != other_program || args != other_args,
                "{action:?} and {other:?} both dispatch {program} {args:?}, so one of them runs \
                 the wrong action"
            );
        }
    }
    // Sanity: the two folder actions really are about the containing directory,
    // which is the part a wrong `path` argument would get wrong.
    assert_eq!(containing_dir(PATH), DIR);
}
