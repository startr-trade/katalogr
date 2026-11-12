mod snapshot_format {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_store::{
        SNAPSHOT_FORMAT, SNAPSHOT_MAGIC, SnapshotManifest, SnapshotPosition, StoreError,
        decode_snapshot, install_snapshot, read_manifest, write_snapshot,
    };
    use sha2::{Digest, Sha256};

    fn dir(t: &tempfile::TempDir) -> Utf8PathBuf {
        Utf8PathBuf::from_path_buf(t.path().to_path_buf()).unwrap()
    }

    /// A deterministic, mildly compressible stand-in for a database file, over a few chunks.
    fn fake_database(path: &Utf8Path) -> Vec<u8> {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut v = Vec::with_capacity(3 * 1024 * 1024 + 17);
        while v.len() < 3 * 1024 * 1024 + 17 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let b = (x >> 24) as u8 & 0x0f;
            v.extend(std::iter::repeat_n(b, 1 + (x as usize & 7)));
        }
        std::fs::write(path, &v).unwrap();
        v
    }

    fn input() -> SnapshotManifest {
        SnapshotManifest {
            format: 0,
            bytes: 0,
            positions: vec![
                SnapshotPosition {
                    repo: "zeta".into(),
                    commit: "b".repeat(40),
                },
                SnapshotPosition {
                    repo: "alpha".into(),
                    commit: "a".repeat(40),
                },
            ],
            facts_version: "f".repeat(64),
            cache_schema: 15,
            digest: "stale".into(),
        }
    }

    fn written(t: &tempfile::TempDir) -> (Utf8PathBuf, Vec<u8>, SnapshotManifest) {
        let d = dir(t);
        let db = d.join("db");
        let bytes = fake_database(&db);
        let out = d.join("x.ksnap");
        let m = write_snapshot(&db, input(), &out).unwrap();
        (out, bytes, m)
    }

    #[test]
    fn manifest_fields_are_exact() {
        let t = tempfile::tempdir().unwrap();
        let (out, bytes, m) = written(&t);
        assert_eq!(m.format, SNAPSHOT_FORMAT);
        assert_eq!(m.bytes, bytes.len() as u64);
        let want: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(m.digest, want);
        assert_eq!(m.cache_schema, 15);
        assert_eq!(m.positions[0].repo, "alpha");
        assert_eq!(m.positions[1].repo, "zeta");
        assert_eq!(read_manifest(&out).unwrap(), m);
        let raw = std::fs::read(&out).unwrap();
        let line = raw.split(|b| *b == b'\n').nth(1).unwrap();
        let v: serde_json::Value = serde_json::from_slice(line).unwrap();
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "bytes",
                "cache_schema",
                "digest",
                "facts_version",
                "format",
                "positions"
            ]
        );
        // The payload decodes back to the same bytes.
        let target = dir(&t).join("decoded");
        decode_snapshot(&out, &m, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
    }

    #[test]
    fn manifest_is_sorted_compact_json() {
        let t = tempfile::tempdir().unwrap();
        let (out, _, m) = written(&t);
        let raw = std::fs::read(&out).unwrap();
        let mut lines = raw.splitn(3, |b| *b == b'\n');
        assert_eq!(
            lines.next().unwrap(),
            format!("{SNAPSHOT_MAGIC} {SNAPSHOT_FORMAT}").as_bytes()
        );
        let line = String::from_utf8(lines.next().unwrap().to_vec()).unwrap();
        let want = format!(
            "{{\"bytes\":{},\"cache_schema\":15,\"digest\":\"{}\",\"facts_version\":\"{}\",\"format\":1,\"positions\":[{{\"commit\":\"{}\",\"repo\":\"alpha\"}},{{\"commit\":\"{}\",\"repo\":\"zeta\"}}]}}",
            m.bytes,
            m.digest,
            "f".repeat(64),
            "a".repeat(40),
            "b".repeat(40)
        );
        assert_eq!(line, want);
        // Writing again gives the same file.
        let again = dir(&t).join("y.ksnap");
        write_snapshot(&dir(&t).join("db"), input(), &again).unwrap();
        assert_eq!(std::fs::read(&again).unwrap(), raw);
        // No temp file is left behind.
        assert!(!dir(&t).join("x.ksnap.part").exists());
    }

    /// A small fixed file: a short pseudo-random database and the fixed manifest.
    fn small_file(t: &tempfile::TempDir) -> (Utf8PathBuf, Vec<u8>) {
        let d = dir(t);
        let db = d.join("small-db");
        let mut x = 0x2545_f491_4f6c_dd1du64;
        let mut v = Vec::new();
        while v.len() < 6000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            v.extend(std::iter::repeat_n(
                (x >> 24) as u8 & 0x03,
                1 + (x as usize & 31),
            ));
        }
        std::fs::write(&db, &v).unwrap();
        let out = d.join("small.ksnap");
        write_snapshot(&db, input(), &out).unwrap();
        let raw = std::fs::read(&out).unwrap();
        (out, raw)
    }

    #[test]
    fn every_single_byte_flip_refused() {
        let t = tempfile::tempdir().unwrap();
        let (out, raw) = small_file(&t);
        let m = read_manifest(&out).unwrap();
        let target = dir(&t).join("flip-target");
        let flipped = dir(&t).join("flipped.ksnap");
        for off in 0..raw.len() {
            for mask in [0xffu8, 0x01] {
                let mut bad = raw.clone();
                bad[off] ^= mask;
                std::fs::write(&flipped, &bad).unwrap();
                let refused = match read_manifest(&flipped) {
                    Err(_) => true,
                    Ok(mm) => decode_snapshot(&flipped, &mm, &target).is_err(),
                };
                assert!(refused, "offset {off} mask {mask:#x} was accepted");
            }
        }
        // The unchanged file still decodes.
        decode_snapshot(&out, &m, &target).unwrap();
    }

    #[test]
    fn appended_byte_refused() {
        let t = tempfile::tempdir().unwrap();
        let (out, mut raw) = small_file(&t);
        let m = read_manifest(&out).unwrap();
        raw.push(0);
        let f = dir(&t).join("appended.ksnap");
        std::fs::write(&f, &raw).unwrap();
        // The checksum already refuses it; a file whose checksum covers the extra byte is
        // refused by the decoder.
        assert!(matches!(read_manifest(&f), Err(StoreError::BadSnapshot(_))));
        let at = raw
            .iter()
            .enumerate()
            .filter(|(_, b)| **b == b'\n')
            .nth(1)
            .unwrap()
            .0
            + 1;
        let mut h = Sha256::new();
        let line_start = raw.iter().position(|b| *b == b'\n').unwrap() + 1;
        let body_start = at + 1 + raw[at + 1..].iter().position(|b| *b == b'\n').unwrap();
        h.update(&raw[line_start..at]);
        h.update(&raw[body_start + 1..]);
        let sum: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        raw[at + 12..at + 12 + 64].copy_from_slice(sum.as_bytes());
        std::fs::write(&f, &raw).unwrap();
        let mm = read_manifest(&f).unwrap();
        assert_eq!(mm, m);
        let target = dir(&t).join("appended-target");
        let e = decode_snapshot(&f, &mm, &target).unwrap_err();
        assert!(e.to_string().contains("1 bytes after its data"), "{e}");
        assert!(!target.exists());
    }

    #[test]
    fn truncated_file_refused() {
        let t = tempfile::tempdir().unwrap();
        let (out, _, m) = written(&t);
        let raw = std::fs::read(&out).unwrap();
        // Cut in the payload.
        let cut = dir(&t).join("cut.ksnap");
        std::fs::write(&cut, &raw[..raw.len() / 2]).unwrap();
        let target = dir(&t).join("decoded");
        let e = decode_snapshot(&cut, &m, &target).unwrap_err();
        assert!(matches!(e, StoreError::BadSnapshot(_)), "{e}");
        assert!(!target.exists());
        // Cut in the manifest line.
        let header = dir(&t).join("head.ksnap");
        std::fs::write(&header, &raw[..40]).unwrap();
        assert!(matches!(
            read_manifest(&header),
            Err(StoreError::BadSnapshot(_))
        ));
        // A refused install leaves no import file and no database.
        let cache = dir(&t).join("cache");
        std::fs::create_dir_all(&cache).unwrap();
        assert!(install_snapshot(&cut, &m, &cache).is_err());
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 0);
    }

    #[test]
    fn oversized_payload_refused() {
        let t = tempfile::tempdir().unwrap();
        let (out, _, mut m) = written(&t);
        m.bytes -= 1;
        let target = dir(&t).join("decoded");
        let e = decode_snapshot(&out, &m, &target).unwrap_err();
        assert!(matches!(e, StoreError::BadSnapshot(_)), "{e}");
        assert!(!target.exists());
        // A short claim and a wrong digest are refused as well.
        let (_, _, mut m) = written(&t);
        m.bytes += 1;
        assert!(decode_snapshot(&out, &m, &target).is_err());
        m.bytes -= 1;
        m.digest = "0".repeat(64);
        assert!(decode_snapshot(&out, &m, &target).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn unknown_format_refused() {
        let t = tempfile::tempdir().unwrap();
        let (out, _, _) = written(&t);
        let mut raw = std::fs::read(&out).unwrap();
        let at = SNAPSHOT_MAGIC.len() + 1;
        raw[at] = b'2';
        let newer = dir(&t).join("newer.ksnap");
        std::fs::write(&newer, &raw).unwrap();
        assert!(matches!(
            read_manifest(&newer),
            Err(StoreError::SnapshotFormat(2))
        ));
        // A wrong magic and a bad digest shape are plain damage.
        raw[0] = b'X';
        std::fs::write(&newer, &raw).unwrap();
        assert!(matches!(
            read_manifest(&newer),
            Err(StoreError::BadSnapshot(_))
        ));
        let mut m = input();
        m.format = 1;
        m.digest = "ABC".into();
        let line = serde_json::to_string(&m).unwrap();
        std::fs::write(&newer, format!("{SNAPSHOT_MAGIC} 1\n{line}\n")).unwrap();
        assert!(matches!(
            read_manifest(&newer),
            Err(StoreError::BadSnapshot(_))
        ));
    }
}
