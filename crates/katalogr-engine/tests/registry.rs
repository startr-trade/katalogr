mod support;

mod registry {
    use super::support::TestExtractor;
    use katalogr_engine::registry::ExtensionRegistry;
    use katalogr_model::{
        ArtifactName, Confidence, ExportKey, ExtractError, ExtractInput, Extractor, Fragment,
        LinkKind, Name, RelPath,
    };

    struct Suffix(&'static str, &'static str);

    impl Extractor for Suffix {
        fn id(&self) -> &'static str {
            self.0
        }
        fn handles(&self, path: &RelPath) -> bool {
            path.as_str().ends_with(self.1)
        }
        fn extract(&self, _: &ExtractInput) -> Result<Fragment, ExtractError> {
            Err(ExtractError::Unsupported("stub".into()))
        }
    }

    #[test]
    fn picks_first_handling_extractor() {
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(Suffix("first", ".tx")));
        reg.register(Box::new(Suffix("second", ".tx")));
        reg.register(Box::new(Suffix("third", ".ty")));
        let p = |s| RelPath::new(s).unwrap();
        assert_eq!(reg.for_path(&p("a/b.tx")).unwrap().id(), "first");
        assert_eq!(reg.for_path(&p("a/b.ty")).unwrap().id(), "third");
        assert!(reg.for_path(&p("a/b.zz")).is_none());
        assert_eq!(reg.ids(), vec!["first", "second", "third"]);
    }

    #[test]
    fn test_extractor_parses_directives() {
        let path = RelPath::new("d/f.tx").unwrap();
        let src = "# comment\n\nsym fn a\nsym fn b\nsym fn a\nref a calls ns:k\nexport ns:b b\nlink a calls b\n";
        let input = ExtractInput {
            repo: "r",
            path: &path,
            revision: "rev1",
            content: src.as_bytes(),
        };
        assert!(TestExtractor.handles(&path));
        let f = TestExtractor.extract(&input).unwrap();
        assert_eq!(f.extension, "test");
        assert_eq!(f.provenance.revision, "rev1");
        let names: Vec<String> = f.symbols.iter().map(|s| s.name.canonical()).collect();
        assert_eq!(names, ["r:d/f.tx#fn:a", "r:d/f.tx#fn:b", "r:d/f.tx#fn:a~2"]);
        assert_eq!(f.symbols[0].span.start_line, 3);
        assert_eq!(f.symbols[0].span.end_col, 8);
        let a = f.symbols[0].name.id();
        let b = f.symbols[1].name.id();
        assert_eq!(f.refs.len(), 1);
        assert_eq!(f.refs[0].from, a);
        assert_eq!(f.refs[0].kind, LinkKind("calls".into()));
        assert_eq!(f.refs[0].confidence, Confidence::Extracted);
        assert_eq!(
            f.refs[0].candidates,
            vec![ExportKey {
                namespace: "ns".into(),
                key: "k".into()
            }]
        );
        assert_eq!(f.exports.len(), 1);
        assert_eq!(f.exports[0].target, b);
        assert_eq!(f.links.len(), 1);
        assert_eq!((f.links[0].from, f.links[0].to), (a, b));
        assert_eq!(f.artifact.name, ArtifactName::new("r", "d/f.tx").unwrap());

        let bad = ExtractInput {
            content: b"sym fn a\nwat\n",
            ..input
        };
        match TestExtractor.extract(&bad) {
            Err(ExtractError::Parse { span: Some(s), .. }) => assert_eq!(s.start_line, 2),
            other => panic!("{other:?}"),
        }
    }
}
