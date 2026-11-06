//! Stored answers (FEAT-096 R-7, R-8, R-9, R-12): one contract over every `AnswerStore`.

mod answer_store {
    use camino::Utf8PathBuf;
    use katalogr_store::{
        AnswerListing, AnswerStore, DirAnswerStore, MAX_ANSWER_BYTES, PruneReport, PutOutcome,
        StoreError, StoredAnswer,
    };
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    fn id(n: u32) -> String {
        format!("v1-{n:032x}")
    }

    fn answer(n: u32, payload: &[u8]) -> StoredAnswer {
        let mut args = BTreeMap::new();
        args.insert("limit".to_string(), "5".to_string());
        StoredAnswer::new(
            id(n),
            format!("val{n}"),
            "hubs".into(),
            args,
            payload.to_vec(),
        )
    }

    /// An in-memory store: the second implementation of the trait.
    #[derive(Default)]
    struct MemoryStore(Mutex<BTreeMap<String, StoredAnswer>>);

    impl AnswerStore for MemoryStore {
        fn get(&self, v: &str) -> Result<Option<StoredAnswer>, StoreError> {
            Ok(self.0.lock().unwrap().get(v).cloned())
        }
        fn put(&self, a: &StoredAnswer) -> Result<PutOutcome, StoreError> {
            if a.payload.len() as u64 > MAX_ANSWER_BYTES {
                return Ok(PutOutcome::TooLarge);
            }
            self.0
                .lock()
                .unwrap()
                .insert(a.header.view_id.clone(), a.clone());
            Ok(PutOutcome::Stored)
        }
        fn remove(&self, v: &str) -> Result<(), StoreError> {
            self.0.lock().unwrap().remove(v);
            Ok(())
        }
        fn list(&self) -> Result<Vec<AnswerListing>, StoreError> {
            Ok(self
                .0
                .lock()
                .unwrap()
                .values()
                .map(|a| AnswerListing {
                    header: a.header.clone(),
                    bytes: a.payload.len() as u64,
                })
                .collect())
        }
        fn prune(&self, max: u64) -> Result<PruneReport, StoreError> {
            let mut m = self.0.lock().unwrap();
            let mut total: u64 = m.values().map(|a| a.payload.len() as u64).sum();
            let mut removed = 0;
            let keys: Vec<String> = m.keys().cloned().collect();
            for k in keys {
                if total <= max {
                    break;
                }
                total -= m.remove(&k).unwrap().payload.len() as u64;
                removed += 1;
            }
            Ok(PruneReport {
                removed,
                bytes_left: total,
            })
        }
    }

    fn contract(s: &dyn AnswerStore) {
        assert_eq!(s.get(&id(1)).unwrap(), None);
        s.remove(&id(1)).unwrap();
        let a = answer(1, b"{\"a\":1}");
        assert_eq!(s.put(&a).unwrap(), PutOutcome::Stored);
        assert_eq!(s.get(&id(1)).unwrap(), Some(a.clone()));
        // Replaces whole.
        let b = answer(1, b"{\"a\":22}");
        s.put(&b).unwrap();
        assert_eq!(s.get(&id(1)).unwrap(), Some(b));
        s.put(&answer(3, b"{}")).unwrap();
        s.put(&answer(2, b"{}")).unwrap();
        let ids: Vec<String> = s
            .list()
            .unwrap()
            .into_iter()
            .map(|l| l.header.view_id)
            .collect();
        assert_eq!(ids, vec![id(1), id(2), id(3)]);
        s.remove(&id(2)).unwrap();
        assert_eq!(s.get(&id(2)).unwrap(), None);
        assert_eq!(s.list().unwrap().len(), 2);
        let r = s.prune(0).unwrap();
        assert_eq!(r.removed, 2);
        assert_eq!(r.bytes_left, 0);
        assert!(s.list().unwrap().is_empty());
    }

    fn dir() -> (tempfile::TempDir, Utf8PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let p = Utf8PathBuf::from_path_buf(t.path().join("answers")).unwrap();
        (t, p)
    }

    #[test]
    fn dir_store_contract() {
        let (_t, p) = dir();
        contract(&DirAnswerStore::new(p));
    }

    #[test]
    fn memory_store_contract() {
        contract(&MemoryStore::default());
    }

    #[test]
    fn bad_view_id_is_refused() {
        let (_t, p) = dir();
        let s = DirAnswerStore::new(p);
        assert!(matches!(s.get("../x"), Err(StoreError::Row(_))));
        assert!(matches!(s.remove("v1-ABC"), Err(StoreError::Row(_))));
    }

    fn file_of(root: &Utf8PathBuf, n: u32) -> Utf8PathBuf {
        let i = id(n);
        root.join(&i[3..5]).join(format!("{i}.answer"))
    }

    #[test]
    fn torn_entry_is_a_miss() {
        let (_t, p) = dir();
        let s = DirAnswerStore::new(p.clone());
        s.put(&answer(1, b"{\"abc\":1}")).unwrap();
        let f = file_of(&p, 1);
        let full = std::fs::read(&f).unwrap();
        // Short payload, no newline at all, zeroed file, and a header for another ID.
        for torn in [
            full[..full.len() - 2].to_vec(),
            full[..10].to_vec(),
            vec![0u8; full.len()],
            Vec::new(),
        ] {
            std::fs::write(&f, &torn).unwrap();
            assert_eq!(s.get(&id(1)).unwrap(), None);
            assert!(!f.exists(), "an unusable entry is removed");
        }
        s.put(&answer(2, b"{}")).unwrap();
        std::fs::copy(file_of(&p, 2), file_of(&p, 1)).unwrap_or_else(|_| {
            std::fs::create_dir_all(file_of(&p, 1).parent().unwrap()).unwrap();
            std::fs::copy(file_of(&p, 2), file_of(&p, 1)).unwrap()
        });
        assert_eq!(s.get(&id(1)).unwrap(), None);
    }

    #[test]
    fn concurrent_puts_leave_one_entry() {
        let (_t, p) = dir();
        let s = DirAnswerStore::new(p.clone());
        let a = answer(7, &vec![b'x'; 100_000]);
        std::thread::scope(|sc| {
            for _ in 0..8 {
                sc.spawn(|| {
                    for _ in 0..20 {
                        s.put(&a).unwrap();
                        if let Some(got) = s.get(&id(7)).unwrap() {
                            assert_eq!(got, a);
                        }
                    }
                });
            }
        });
        assert_eq!(s.get(&id(7)).unwrap(), Some(a));
        assert_eq!(s.list().unwrap().len(), 1);
        let dir = file_of(&p, 7).parent().unwrap().to_owned();
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1, "no temp left");
    }

    #[test]
    fn prune_removes_oldest() {
        let (_t, p) = dir();
        let s = DirAnswerStore::new(p.clone());
        for n in [3, 1, 2] {
            s.put(&answer(n, b"{}")).unwrap();
        }
        let base = std::time::SystemTime::now() - std::time::Duration::from_secs(1000);
        // Entry 2 is oldest, then 3, then 1.
        for (n, age) in [(2, 0u64), (3, 10), (1, 20)] {
            let f = std::fs::File::options()
                .write(true)
                .open(file_of(&p, n))
                .unwrap();
            f.set_modified(base + std::time::Duration::from_secs(age))
                .unwrap();
        }
        let total: u64 = s.list().unwrap().iter().map(|l| l.bytes).sum();
        let one = total / 3;
        let r = s.prune(total - one).unwrap();
        assert_eq!(r.removed, 1);
        assert_eq!(r.bytes_left, total - one);
        assert_eq!(s.get(&id(2)).unwrap(), None);
        assert!(s.get(&id(3)).unwrap().is_some());
        // Equal ages break by view ID.
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(500);
        for n in [1, 3] {
            std::fs::File::options()
                .write(true)
                .open(file_of(&p, n))
                .unwrap()
                .set_modified(t)
                .unwrap();
        }
        let r = s.prune(one).unwrap();
        assert_eq!(r.removed, 1);
        assert_eq!(s.get(&id(1)).unwrap(), None);
        assert!(s.get(&id(3)).unwrap().is_some());
        // Old temp files go; fresh ones stay.
        let dir = file_of(&p, 3).parent().unwrap().to_owned();
        let old = dir.join(".tmp-1-1-1");
        let new = dir.join(".tmp-2-2-2");
        std::fs::write(&old, b"x").unwrap();
        std::fs::write(&new, b"x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
            .unwrap();
        s.prune(u64::MAX).unwrap();
        assert!(!old.exists());
        assert!(new.exists());
    }

    #[test]
    fn oversize_entry_not_stored() {
        let (_t, p) = dir();
        let s = DirAnswerStore::new(p.clone());
        let big = answer(9, &vec![b'x'; MAX_ANSWER_BYTES as usize + 1]);
        assert_eq!(s.put(&big).unwrap(), PutOutcome::TooLarge);
        assert_eq!(s.get(&id(9)).unwrap(), None);
        assert!(!p.exists() || s.list().unwrap().is_empty());
    }
}
