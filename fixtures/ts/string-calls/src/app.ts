import express from "express";
import { show } from "./handlers/users";
import * as users from "./handlers/users";
import { ctrl } from "./handlers/ctrl";

const app = express();

function auth(req: Request, res: Response) {}

function health(req: Request, res: Response) {}

app.get("/health", health);
app.get("/users/:id", auth, show);
app.post("/users", users.create);
app.get("/ctrl", ctrl.list);
app.get("/inline", (req, res) => res.end());
app.get("/function", function (req, res) {});
app.get("setting");
