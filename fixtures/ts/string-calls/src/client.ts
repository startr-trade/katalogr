export async function load(id: number) {
  const r = await fetch(`/v1/users/${id}`, { method: "POST", headers: { a: "b" } });
  const s = await axios.get("/v1/ping");
  const t = await axios({ url: "/x", method: "post" });
  const u = await fetch("/a" + id + "/b");
  console.log("loaded the data");
  return [r, s, t, u];
}

export class Svc {
  constructor(private http: HttpClient) {}

  list() {
    return this.http.get("/v1/items");
  }
}

fetch("/boot");
