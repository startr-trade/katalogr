//! Working-set tests that need the composition root (FEAT-039).

mod working_set {
    use katalogr_dist::working_set::{EDIT_LOG, append_edit, read_edits};
    use katalogr_model::RelPath;

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    #[test]
    fn edit_log_bounded_and_deduped() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cache");
        assert!(read_edits(&dir).is_empty());
        for p in ["a", "b", "c", "a"] {
            append_edit(&dir, &rel(p), 3).unwrap();
        }
        // Deduplicated on append: `a` moved to the end.
        assert_eq!(read_edits(&dir), [rel("b"), rel("c"), rel("a")]);
        // Bounded to the last `max`, newest last.
        append_edit(&dir, &rel("d"), 3).unwrap();
        assert_eq!(read_edits(&dir), [rel("c"), rel("a"), rel("d")]);
        append_edit(&dir, &rel("e"), 1).unwrap();
        assert_eq!(read_edits(&dir), [rel("e")]);
        // Plain lines, and no temp file left behind.
        assert_eq!(std::fs::read_to_string(dir.join(EDIT_LOG)).unwrap(), "e\n");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, [EDIT_LOG]);
    }
}
