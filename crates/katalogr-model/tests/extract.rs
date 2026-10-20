mod model {
    use katalogr_model::ExtractError;

    #[test]
    fn unsupported_message_recognised() {
        let msg = ExtractError::Unsupported("x".into()).to_string();
        assert!(ExtractError::is_unsupported_message(&msg));
        let parse = ExtractError::Parse {
            span: None,
            message: "x".into(),
        }
        .to_string();
        assert!(!ExtractError::is_unsupported_message(&parse));
        assert!(!ExtractError::is_unsupported_message("boom"));
    }
}
