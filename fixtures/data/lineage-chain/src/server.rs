fn place() {}

pub fn routes(app: Router) -> Router {
    app.route("/orders", post(place))
}
