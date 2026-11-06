mod support;

mod map {
    use super::support::*;
    use katalogr_query::map;

    #[test]
    fn fields_not_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = open(&tmp, &["r"]);
        {
            let mut tx = cache.write().unwrap();
            let a = artifact_with(
                &mut tx,
                "r",
                "a.tx",
                vec![
                    sym(&[("type", "Holder")], "type"),
                    field(&[("type", "Holder"), ("field", "x")], "string", true, false),
                    sym(&[("fn", "user")], "fn"),
                ],
            );
            link(&mut tx, "r", "a.tx", a[0], "contains", a[1]);
            link(&mut tx, "r", "a.tx", a[2], "calls", a[1]);
            tx.commit().unwrap();
        }
        let tx = cache.read().unwrap();
        let entries = map(&tx).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.symbol.name.as_str()).collect();
        assert!(names.contains(&"r:a.tx#type:Holder") && names.contains(&"r:a.tx#fn:user"));
        assert!(!names.iter().any(|n| n.contains("field:")), "{names:?}");
    }
}
