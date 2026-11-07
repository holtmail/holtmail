use ed25519_dalek::{Signer, SigningKey};
use holtmail_category::Topic;
use holtmail_category_data::{Header, MANIFEST_FORMAT, MAX_FILE_BYTES, ManifestFile, Row, write};

use super::*;

const NOW: i64 = 1_794_009_600;
const DAY: i64 = 24 * 60 * 60;
const TEST: ReleaseKey = ReleaseKey {
    id: "test",
    public: TEST_PUBLIC,
};
const WIKIDATA: &str = "wikidata-categories.tsv.gz";
const UT1: &str = "ut1-categories.tsv.gz";

fn seed() -> [u8; 32] {
    let hex = include_str!("../../tests/fixtures/test-signing-key.hex").trim();
    assert_eq!(hex.len(), 64);
    let mut seed = [0u8; 32];
    for (i, byte) in seed.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    seed
}

fn sign(bytes: &[u8]) -> Vec<u8> {
    SigningKey::from_bytes(&seed())
        .sign(bytes)
        .to_bytes()
        .to_vec()
}

fn file(name: &str, version: &str, rows: &[&str]) -> Vec<u8> {
    let rows: Vec<Row> = rows
        .iter()
        .map(|domain| Row {
            domain: (*domain).into(),
            topics: [Topic::Purchases].into_iter().collect(),
            source: "test".into(),
        })
        .collect();
    write(&Header::new(name, version), &rows).unwrap()
}

fn entry(name: &str, bytes: &[u8], rows: u64) -> ManifestFile {
    ManifestFile {
        name: name.into(),
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        license: holtmail_category_data::data_file(name)
            .unwrap()
            .license
            .into(),
        rows,
    }
}

fn wikidata() -> Vec<u8> {
    file(
        "wikidata",
        "2026110801",
        &["example.com", "shop.example.org"],
    )
}

fn ut1() -> Vec<u8> {
    file("ut1", "2026110801", &["example.net"])
}

fn manifest() -> Manifest {
    Manifest {
        format: MANIFEST_FORMAT,
        version: "2026110801".into(),
        key_id: "test".into(),
        not_after: NOW + 30 * DAY,
        files: vec![entry(WIKIDATA, &wikidata(), 2), entry(UT1, &ut1(), 1)],
    }
}

fn verify(
    bytes: &[u8],
    signature: &[u8],
    now: i64,
    loaded: Option<&str>,
) -> Result<Manifest, UpdateError> {
    fresh(verify_with(&[TEST], bytes, signature)?, now, loaded)
}

fn signed(m: &Manifest) -> (Vec<u8>, Vec<u8>) {
    let json = m.to_json();
    let signature = sign(&json);
    (json, signature)
}

fn refused(edit: impl FnOnce(&mut Manifest), now: i64, loaded: Option<&str>) -> UpdateError {
    let mut m = manifest();
    edit(&mut m);
    let (json, signature) = signed(&m);
    verify(&json, &signature, now, loaded).unwrap_err()
}

#[test]
fn fixture_seed_gives_the_test_key() {
    let key = SigningKey::from_bytes(&seed());
    assert_eq!(key.verifying_key().to_bytes(), TEST_PUBLIC);
}

#[test]
fn accepts_a_signed_manifest() {
    let (json, signature) = signed(&manifest());
    assert_eq!(verify(&json, &signature, NOW, None).unwrap(), manifest());
    assert_eq!(
        verify(&json, &signature, NOW, Some("2026110701")).unwrap(),
        manifest()
    );
    assert_eq!(verify_with(&[TEST], &json, &signature).unwrap(), manifest());
}

#[test]
fn signature_refusals() {
    let (json, signature) = signed(&manifest());
    assert_eq!(
        verify(&json, &signature[..63], NOW, None),
        Err(UpdateError::SignatureLength)
    );
    assert_eq!(
        verify(&json, &[signature.clone(), vec![0]].concat(), NOW, None),
        Err(UpdateError::SignatureLength)
    );
    assert_eq!(
        verify(&json, &[], NOW, None),
        Err(UpdateError::SignatureLength)
    );
    assert_eq!(
        verify(&json, &[0u8; 64], NOW, None),
        Err(UpdateError::Signature)
    );
    for index in [0, 10, json.len() / 2, json.len() - 2] {
        let mut flipped = json.clone();
        flipped[index] ^= 0x01;
        assert!(
            verify(&flipped, &signature, NOW, None).is_err(),
            "byte {index}"
        );
    }
    let mut changed = json.clone();
    let at = changed.windows(4).position(|w| w == b"2026").unwrap();
    changed[at + 3] = b'7';
    assert_eq!(
        verify(&changed, &signature, NOW, None),
        Err(UpdateError::Signature)
    );
    for index in [0, 31, 32, 63] {
        let mut bad = signature.clone();
        bad[index] ^= 0x80;
        assert_eq!(
            verify(&json, &bad, NOW, None),
            Err(UpdateError::Signature),
            "signature byte {index}"
        );
    }
    let mut reindented = json.clone();
    reindented.insert(1, b' ');
    assert_eq!(
        verify(&reindented, &signature, NOW, None),
        Err(UpdateError::Signature)
    );
}

#[test]
fn key_refusals() {
    let mut m = manifest();
    m.key_id = "release-1".into();
    let (json, signature) = signed(&m);
    assert_eq!(
        verify(&json, &signature, NOW, None),
        Err(UpdateError::NoReleaseKey)
    );
    let other = SigningKey::from_bytes(&[7u8; 32]);
    let other_key = ReleaseKey {
        id: "other",
        public: other.verifying_key().to_bytes(),
    };
    m.key_id = "other".into();
    let json = m.to_json();
    let theirs = other.sign(&json).to_bytes();
    assert!(verify_with(&[TEST, other_key], &json, &theirs).is_ok());
    let mine = sign(&json);
    assert_eq!(
        verify_with(&[TEST, other_key], &json, &mine),
        Err(UpdateError::Signature)
    );
    let swapped = ReleaseKey {
        id: "other",
        public: TEST_PUBLIC,
    };
    assert_eq!(
        verify_with(&[swapped], &json, &theirs),
        Err(UpdateError::Signature)
    );
    assert_eq!(
        verify(b"{\"key_id\":\"Test\"}", &[0u8; 64], NOW, None),
        Err(UpdateError::Manifest(ManifestError::KeyId))
    );
    assert_eq!(
        verify(b"not json", &[0u8; 64], NOW, None),
        Err(UpdateError::Manifest(ManifestError::Json))
    );
}

#[test]
fn signed_but_invalid_manifest_is_refused() {
    assert_eq!(
        refused(|m| m.files[1].name = "other.tsv.gz".into(), NOW, None),
        UpdateError::Manifest(ManifestError::FileName)
    );
    assert_eq!(
        refused(|m| m.files[1].name = WIKIDATA.into(), NOW, None),
        UpdateError::Manifest(ManifestError::DuplicateFile)
    );
    assert_eq!(
        refused(|m| m.format = 2, NOW, None),
        UpdateError::Manifest(ManifestError::Format)
    );
    let mut big = manifest().to_json();
    big.resize(holtmail_category_data::MAX_MANIFEST_BYTES + 1, b' ');
    let signature = sign(&big);
    assert_eq!(
        verify(&big, &signature, NOW, None),
        Err(UpdateError::Manifest(ManifestError::TooLarge))
    );
}

#[test]
fn expiry() {
    assert_eq!(
        refused(|m| m.not_after = NOW - 1, NOW, None),
        UpdateError::Expired
    );
    assert_eq!(
        refused(|m| m.not_after = NOW + MAX_VALIDITY_SECONDS + 1, NOW, None),
        UpdateError::TooFarAhead
    );
    for not_after in [NOW, NOW + MAX_VALIDITY_SECONDS] {
        let mut m = manifest();
        m.not_after = not_after;
        let (json, signature) = signed(&m);
        assert!(verify(&json, &signature, NOW, None).is_ok());
    }
    let (json, signature) = signed(&manifest());
    assert_eq!(
        verify(&json, &signature, NOW + 31 * DAY, None),
        Err(UpdateError::Expired)
    );
    assert_eq!(
        verify(&json, &signature, i64::MIN, None),
        Err(UpdateError::TooFarAhead)
    );
    assert!(verify_with(&[TEST], &json, &signature).is_ok());
}

#[test]
fn rollback() {
    let (json, signature) = signed(&manifest());
    for loaded in ["2026110801", "2026110802", "20261109000", "v1"] {
        assert_eq!(
            verify(&json, &signature, NOW, Some(loaded)),
            Err(UpdateError::NotNewer),
            "{loaded}"
        );
    }
    assert!(verify(&json, &signature, NOW, Some("1")).is_ok());
}

#[test]
fn files_are_checked_against_the_manifest() {
    let m = manifest();
    let table = check_file(&m, WIKIDATA, &wikidata()).unwrap();
    assert_eq!(table.name(), "wikidata");
    assert_eq!(table.len(), 2);
    assert_eq!(check_file(&m, UT1, &ut1()).unwrap().name(), "ut1");

    assert_eq!(
        check_file(&m, "other.tsv.gz", &wikidata()).unwrap_err(),
        UpdateError::FileName
    );
    assert_eq!(
        check_file(&m, "../wikidata-categories.tsv.gz", &wikidata()).unwrap_err(),
        UpdateError::FileName
    );
    let mut only = m.clone();
    only.files.truncate(1);
    assert_eq!(
        check_file(&only, UT1, &ut1()).unwrap_err(),
        UpdateError::NotListed
    );

    let mut longer = wikidata();
    longer.push(0);
    assert_eq!(
        check_file(&m, WIKIDATA, &longer).unwrap_err(),
        UpdateError::Size
    );
    let mut flipped = wikidata();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x01;
    assert_eq!(
        check_file(&m, WIKIDATA, &flipped).unwrap_err(),
        UpdateError::Sha256
    );
    assert_eq!(
        check_file(&m, WIKIDATA, &ut1()).unwrap_err(),
        UpdateError::Size
    );

    let listed = |name: &str, bytes: &[u8], rows: u64| {
        let mut m = manifest();
        m.files = vec![entry(name, bytes, rows)];
        m
    };
    let wrong_name = file("ut1", "2026110801", &["example.com"]);
    assert_eq!(
        check_file(&listed(WIKIDATA, &wrong_name, 1), WIKIDATA, &wrong_name).unwrap_err(),
        UpdateError::TableName
    );
    let wrong_version = file("wikidata", "2026110701", &["example.com"]);
    assert_eq!(
        check_file(
            &listed(WIKIDATA, &wrong_version, 1),
            WIKIDATA,
            &wrong_version
        )
        .unwrap_err(),
        UpdateError::TableVersion
    );
    assert_eq!(
        check_file(&listed(WIKIDATA, &wikidata(), 3), WIKIDATA, &wikidata()).unwrap_err(),
        UpdateError::Rows
    );
    let garbage = b"not gzip at all".to_vec();
    assert_eq!(
        check_file(&listed(WIKIDATA, &garbage, 0), WIKIDATA, &garbage).unwrap_err(),
        UpdateError::Data(DataError::Gzip)
    );
}

#[test]
fn oversized_files_are_refused() {
    let mut m = manifest();
    m.files[0].size = MAX_FILE_BYTES;
    let big = vec![0u8; MAX_FILE_BYTES as usize + 1];
    assert_eq!(
        check_file(&m, WIKIDATA, &big).unwrap_err(),
        UpdateError::Size
    );

    let bomb = bomb();
    m.files = vec![entry(WIKIDATA, &bomb, 1)];
    assert_eq!(
        check_file(&m, WIKIDATA, &bomb).unwrap_err(),
        UpdateError::Data(DataError::TooLarge)
    );
}

fn bomb() -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder
        .write_all(b"#holtmail-categories 1 wikidata 2026110801\n")
        .unwrap();
    let chunk = vec![b'a'; 1024 * 1024];
    for _ in 0..33 {
        encoder.write_all(&chunk).unwrap();
    }
    encoder.finish().unwrap()
}

#[test]
fn compiled_keys() {
    let ids: Vec<&str> = keys().iter().map(|key| key.id).collect();
    assert!(CURRENT_KEY.is_none() && NEXT_KEY.is_none());
    let mut m = manifest();
    let (json, signature) = signed(&m);
    if cfg!(feature = "test-key") {
        assert_eq!(ids, ["test"]);
        assert_eq!(verify_stored(&json, &signature).unwrap(), m);
        assert!(verify_manifest(&json, &signature, NOW, None).is_ok());
    } else {
        assert!(ids.is_empty());
        assert_eq!(
            verify_stored(&json, &signature),
            Err(UpdateError::NoReleaseKey)
        );
        assert_eq!(
            verify_manifest(&json, &signature, NOW, None),
            Err(UpdateError::NoReleaseKey)
        );
    }
    m.key_id = "release".into();
    let (json, signature) = signed(&m);
    assert_eq!(
        verify_manifest(&json, &signature, NOW, None),
        Err(UpdateError::NoReleaseKey)
    );
}

#[test]
fn errors_name_rules_only() {
    assert_eq!(
        UpdateError::NoReleaseKey.to_string(),
        "no release signing key is compiled in"
    );
    assert_eq!(
        UpdateError::Manifest(ManifestError::FileName).to_string(),
        "manifest lists a file name outside the fixed set"
    );
    assert_eq!(
        UpdateError::Data(DataError::Line {
            line: 3,
            rule: "order"
        })
        .to_string(),
        "data file: category data line 3: order"
    );
}
