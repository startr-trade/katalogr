use crate::handlers::remove;
use crate::handlers::show;

pub struct Api;

fn local_handler() {}

impl Api {
    fn list() {}

    pub fn router() -> Router {
        Router::new()
            .route("/users/{id}", get(show).delete(remove))
            .route("/local", get(local_handler))
            .route("/list", post(Self::list))
            .route("/inline", get(|| async { "ok" }))
    }
}

pub fn client(base: &str, id: u32) {
    let client = reqwest::Client::new();
    let _ = client.get("https://api.example.test/users");
    let _ = client.post(format!("{}/users/{id}", base));
    let _ = reqwest::get("https://api.example.test/ping".to_string());
    log::info("a message with spaces");
}
