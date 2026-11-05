//! Working-set tests (FEAT-039 T1): tiers, boost and hash, all in memory.

mod working_set {
    use katalogr_model::RelPath;
    use katalogr_query::{Tier, Weights, WorkingSet, boost};

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).unwrap()
    }

    #[test]
    fn strongest_tier_wins() {
        let mut ws = WorkingSet::default();
        ws.add("r", rel("a"), Tier::Branch);
        ws.add("r", rel("a"), Tier::Dirty);
        ws.add("r", rel("a"), Tier::Item);
        ws.add("r", rel("b"), Tier::Item);
        ws.add("r", rel("b"), Tier::Recent);
        assert_eq!(ws.tier("r", &rel("a")), Some(Tier::Dirty));
        assert_eq!(ws.tier("r", &rel("b")), Some(Tier::Recent));
        let c = ws.counts();
        assert_eq!((c.dirty, c.recent, c.branch, c.item_paths), (1, 1, 0, 0));
    }

    #[test]
    fn empty_set_changes_nothing() {
        let ws = WorkingSet::default();
        let w = Weights::default();
        for v in [0.0, 1.0 / 3.0, 12345.678_901_234_5, f64::MIN_POSITIVE] {
            let out = boost(v, "r", &rel("a"), &ws, &w);
            assert_eq!(out.to_bits(), v.to_bits());
        }
        // A path outside a non-empty set is unchanged too.
        let mut ws = WorkingSet::default();
        ws.add("r", rel("x"), Tier::Dirty);
        let v = 1.0 / 3.0;
        assert_eq!(boost(v, "r", &rel("a"), &ws, &w).to_bits(), v.to_bits());
    }

    #[test]
    fn boost_rounds_to_six() {
        let mut ws = WorkingSet::default();
        ws.add("r", rel("a"), Tier::Dirty);
        let w = Weights {
            dirty: 0.5,
            recent: 0.0,
            branch: 0.0,
            item: 0.0,
        };
        // 1/3 * 1.5 = 0.5 exactly after rounding; 0.123456789 * 1.5 = 0.1851851835 -> 0.185185.
        assert_eq!(boost(0.123_456_789, "r", &rel("a"), &ws, &w), 0.185_185);
        assert_eq!(boost(2.0, "r", &rel("a"), &ws, &w), 3.0);
    }

    #[test]
    fn hash_is_order_free() {
        let mut a = WorkingSet::default();
        a.add("r", rel("a"), Tier::Dirty);
        a.add("r", rel("b"), Tier::Branch);
        a.add("s", rel("a"), Tier::Item);
        let mut b = WorkingSet::default();
        b.add("s", rel("a"), Tier::Item);
        b.add("r", rel("b"), Tier::Item);
        b.add("r", rel("b"), Tier::Branch);
        b.add("r", rel("a"), Tier::Dirty);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.hash().len(), 64);
        b.add("r", rel("c"), Tier::Recent);
        assert_ne!(a.hash(), b.hash());
        assert_ne!(WorkingSet::default().hash(), a.hash());
    }
}
