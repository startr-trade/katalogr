"""The GitHub release body from a version's notes (FEAT-114).

The release page has its own title, so the notes' first `# ` heading is dropped. There is no book
around the page, so links to other book pages are made absolute to the documentation site.
Usage: python3 release-notes-body.py <docs/src/releases/vX.Y.Z.md> <out.md>
"""
import re
import sys

SITE = "https://katalogr.startr.trade"


def body(text: str) -> str:
    text = re.sub(r"\A\s*# [^\n]*\n+", "", text, count=1)

    def link(m: "re.Match[str]") -> str:
        target = m.group(2)
        if re.match(r"[a-z][a-z0-9+.-]*:|#|/", target):
            return m.group(0)
        path, _, frag = target.partition("#")
        path = re.sub(r"\.md$", ".html", path)
        path = path[3:] if path.startswith("../") else "releases/" + path
        return f"{m.group(1)}({SITE}/{path}{'#' + frag if frag else ''})"

    text = re.sub(r"(\[[^\]]*\])\(([^)\s]+)\)", link, text)
    return text.rstrip("\n") + f"\n\n---\n\nEvery release's notes: [{SITE}/releases/]({SITE}/releases/index.html).\n"


if __name__ == "__main__":
    src, out = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as f:
        text = f.read()
    with open(out, "w", encoding="utf-8", newline="\n") as f:
        f.write(body(text))
