use std::path::Path;

pub struct Repo {
    root: String,
}

impl Repo {
    pub fn path(&self) -> &str {
        &self.root
    }

    pub fn find(&self, needle: &str) -> bool {
        self.root.contains(needle)
    }

    pub fn read(&self) -> String {
        self.root.clone()
    }

    pub fn write(&mut self, text: &str) {
        self.root = text.to_string();
    }

    pub fn publish(&self) -> usize {
        self.root.len()
    }
}

pub fn score(n: u32) -> u32 {
    n * 2
}

pub fn user_01(p: &Path, text: &str, repo: &mut Repo) {
    let _ = repo.read();
    repo.write(text);
    let _ = p.path();
    let _ = text.find("x");
    let _ = score(1);
    let _ = repo.publish();
}

pub fn user_02(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
    let _ = score(2);
    let _ = repo.publish();
}

pub fn user_03(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
    let _ = score(3);
    let _ = repo.publish();
}

pub fn user_04(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
    let _ = score(4);
    let _ = repo.publish();
}

pub fn user_05(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
    let _ = score(5);
    let _ = repo.publish();
}

pub fn user_06(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
    let _ = score(6);
}

pub fn user_07(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
}

pub fn user_08(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
}

pub fn user_09(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
}

pub fn user_10(p: &Path, text: &str, repo: &Repo) {
    let _ = p.path();
    let _ = text.find("x");
}
