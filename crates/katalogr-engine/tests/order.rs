//! FEAT-047 R-12: the index depends on the final file contents, not on the order, repetition
//! or timing of change events.
mod support;

use camino::Utf8PathBuf;
use katalogr_engine::{EventKind, NewEvent};
use katalogr_model::RelPath;
use proptest::prelude::*;
use support::{cache_dump, drained_graph, fresh_index, init_project, open_engine_threads};

const PATHS: [&str; 4] = ["a.tx", "b.tx", "sub/c.tx", "d.tx"];
const CONTENTS: [&str; 5] = [
    "sym fn A\nexport k:a A\n",
    "sym fn B\nref B calls k:a\n",
    "sym fn A\nsym fn Z\nlink A calls Z\n",
    "sym fn C\nref C calls k:b\nexport k:b C\n",
    "sym struct S\n# a comment\n",
];

/// One step: a file gets new content (`Some`) or is deleted (`None`), its event is appended,
/// and the queue is drained afterwards when `drain` is set.
#[derive(Clone, Debug)]
struct Step {
    path: usize,
    content: Option<usize>,
    drain: bool,
    repeat: u8,
}

fn step() -> impl Strategy<Value = Step> {
    (
        0..PATHS.len(),
        prop::option::weighted(0.8, 0..CONTENTS.len()),
        any::<bool>(),
        1u8..4,
    )
        .prop_map(|(path, content, drain, repeat)| Step {
            path,
            content,
            drain,
            repeat,
        })
}

/// Apply `steps` and `replay` to an engine with `threads` threads, then compare its drained
/// graph with a fresh (sequential) index of the final files.
fn order_independent(
    steps: &[Step],
    replay: &[usize],
    threads: usize,
) -> Result<(), TestCaseError> {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(work.join("sub")).unwrap();
    let kx = root.join("kx");
    init_project(&kx);
    let e = open_engine_threads(&kx, &work, threads);
    let rel = |i: usize| RelPath::new(PATHS[i]).unwrap();

    for s in steps {
        let file = work.join(PATHS[s.path]);
        let kind = match s.content {
            Some(c) => {
                std::fs::write(&file, CONTENTS[c]).unwrap();
                EventKind::Changed
            }
            None => {
                let _ = std::fs::remove_file(&file);
                EventKind::Deleted
            }
        };
        // The file is written before its event, and the event may be delivered again.
        for _ in 0..s.repeat {
            e.inbox()
                .append(&NewEvent::file("r", &rel(s.path), kind))
                .unwrap();
        }
        if s.drain {
            drained_graph(&e);
        }
    }
    // Late duplicates of events for the final state, in an arbitrary order.
    for i in replay {
        e.inbox()
            .append(&NewEvent::file("r", &rel(*i), EventKind::Changed))
            .unwrap();
    }
    let incremental = drained_graph(&e);
    let (_, fresh) = fresh_index(&root, "fresh", &work);
    prop_assert_eq!(incremental, fresh);
    Ok(())
}

mod queue {
    use super::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(24))]

        #[test]
        fn order_independent_results(
            steps in prop::collection::vec(step(), 1..14),
            replay in prop::collection::vec(0..PATHS.len(), 0..6),
        ) {
            order_independent(&steps, &replay, 1)?;
        }

        #[test]
        fn order_independent_results_parallel(
            steps in prop::collection::vec(step(), 1..14),
            replay in prop::collection::vec(0..PATHS.len(), 0..6),
        ) {
            order_independent(&steps, &replay, 4)?;
        }
    }

    const SKIP_PATHS: [&str; 6] = ["a.tx", "b.tx", "c.tx", "d.tx", "e.tx", "f.tx"];
    const SKIP_CONTENTS: [&str; 7] = [
        "sym fn S\nexport k:a S\n",
        "sym fn S\nexport k:a S\nexport k:b S\n",
        "sym fn S\nref S calls k:a\n",
        "sym fn S\nrefs S calls k:b k:a\nref S uses k:a\n",
        "sym fn S\nboundref S consumes ch:x k:a\n",
        "sym fn S\nsym fn T\nexport k:b T\nrefs S calls k:a k:b\n",
        "sym fn S\n",
    ];

    fn run(steps: &[(usize, Option<usize>)], all: bool) -> support::CacheDump {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let kx = root.join("kx");
        init_project(&kx);
        let e = open_engine_threads(&kx, &work, 1);
        e.stitcher().set_resolve_all(all);
        for (p, c) in steps {
            let file = work.join(SKIP_PATHS[*p]);
            let kind = match c {
                Some(c) => {
                    std::fs::write(&file, SKIP_CONTENTS[*c]).unwrap();
                    EventKind::Changed
                }
                None => {
                    let _ = std::fs::remove_file(&file);
                    EventKind::Deleted
                }
            };
            e.inbox()
                .append(&NewEvent::file(
                    "r",
                    &RelPath::new(SKIP_PATHS[*p]).unwrap(),
                    kind,
                ))
                .unwrap();
            drained_graph(&e);
        }
        cache_dump(&e)
            .into_iter()
            .filter(|(n, _)| n != "shard_hashes")
            .collect()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        #[test]
        fn fan_key_skip_equals_full_resolve(
            steps in prop::collection::vec(
                (0..SKIP_PATHS.len(), prop::option::weighted(0.85, 0..SKIP_CONTENTS.len())),
                1..16,
            ),
        ) {
            prop_assert_eq!(run(&steps, false), run(&steps, true));
        }
    }
}
