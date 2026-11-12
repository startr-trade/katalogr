//! The view manifest (FEAT-033 T3).

mod web_manifest {
    use katalogr_web::manifest::manifest;

    const KNOWN: [&str; 8] = [
        "impact",
        "deps",
        "vulnerabilities",
        "type-lineage",
        "git-history",
        "drift",
        "rules",
        "metrics",
    ];

    #[test]
    fn manifest_parses() {
        let m = manifest();
        assert_eq!(m.format, 1);
        assert_eq!(m.wait_seconds, 10);
        assert!(!m.views.is_empty());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(katalogr_web::VIEWS_JSON).unwrap()["views"]
                .as_array()
                .unwrap()
                .len(),
            m.views.len()
        );
    }

    #[test]
    fn link_views_are_exactly_three() {
        let ids: Vec<String> = manifest()
            .views
            .into_iter()
            .filter(|v| v.link)
            .map(|v| v.id)
            .collect();
        assert_eq!(ids, ["graph", "service-map", "impact"]);
    }

    #[test]
    fn standalone_views_have_no_link() {
        for v in manifest().views.iter().filter(|v| !v.link) {
            assert!(
                !["graph", "service-map", "impact"].contains(&v.id.as_str()),
                "{} is a link view",
                v.id
            );
        }
        let health = manifest()
            .views
            .into_iter()
            .find(|v| v.id == "health")
            .unwrap();
        assert!(!health.link && health.nav && health.kind == "panels");
        assert!(health.panels.iter().filter(|p| p.op.is_some()).count() == 5);
    }

    #[test]
    fn needs_names_are_known() {
        let m = manifest();
        let check = |where_: &str, needs: &Option<String>, absent: &Option<String>| {
            if let Some(n) = needs {
                assert!(KNOWN.contains(&n.as_str()), "{where_}: unknown need {n}");
                assert!(
                    absent.as_deref().is_some_and(|a| !a.is_empty()),
                    "{where_}: no absent text"
                );
            }
        };
        for v in &m.views {
            check(&v.id, &v.needs, &v.absent);
            for p in &v.panels {
                check(&format!("{}/{}", v.id, p.id), &p.needs, &p.absent);
            }
        }
    }
}
