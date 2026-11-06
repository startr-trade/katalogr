//! The type lineage row of the view manifest (FEAT-101 T13).

mod web_manifest {
    use katalogr_web::manifest::manifest;

    #[test]
    fn type_lineage_has_no_link() {
        let row = manifest()
            .views
            .into_iter()
            .find(|v| v.id == "type-lineage")
            .expect("the type lineage row");
        assert!(!row.link, "standalone: no monitor link");
        assert!(row.nav);
        assert_eq!(row.kind, "lineage");
        assert_eq!(row.needs.as_deref(), Some("type-lineage"));
        assert_eq!(
            row.absent.as_deref(),
            Some("Type lineage isn't in this version of katalogr.")
        );
    }
}
