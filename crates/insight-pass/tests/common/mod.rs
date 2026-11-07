#![allow(dead_code)]

use std::io::{Cursor, Write};

use bytes::Bytes;
use holtmail_insight::{AttachmentMeta, InsightInput};
use serde_json::Value;
use zip::CompressionMethod;
use zip::write::{SimpleFileOptions, ZipWriter};

pub const PKPASS: &str = "application/vnd.apple.pkpass";

pub fn zip(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(method);
    for (name, data) in entries {
        writer.start_file(*name, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

pub fn pkpass(entries: &[(&str, &[u8])]) -> Vec<u8> {
    zip(entries, CompressionMethod::Deflated)
}

/// Overwrites a little-endian field of every local (`PK\x03\x04`) and central (`PK\x01\x02`)
/// header. Meant for one-entry archives whose data holds no signature bytes.
pub fn patch(mut bytes: Vec<u8>, local_at: usize, central_at: usize, value: &[u8]) -> Vec<u8> {
    let starts: Vec<(usize, usize)> = bytes
        .windows(4)
        .enumerate()
        .filter_map(|(at, window)| match window {
            b"PK\x03\x04" => Some((at, local_at)),
            b"PK\x01\x02" => Some((at, central_at)),
            _ => None,
        })
        .collect();
    for (start, offset) in starts {
        bytes[start + offset..start + offset + value.len()].copy_from_slice(value);
    }
    bytes
}

pub fn boarding_json() -> String {
    r#"{
  "formatVersion": 1,
  "passTypeIdentifier": "pass.com.example.air.boarding",
  "serialNumber": "XA-2026-0001",
  "teamIdentifier": "EXAMPLE01",
  "organizationName": "Example Air",
  "description": "Boarding pass FRA to LIS",
  "relevantDate": "2026-11-20T07:15+01:00",
  "boardingPass": {
    "transitType": "PKTransitTypeAir",
    "primaryFields": [
      {"key": "origin", "label": "Frankfurt", "value": "FRA"},
      {"key": "destination", "label": "Lisbon", "value": "LIS"}
    ]
  },
  "barcodes": [
    {"format": "PKBarcodeFormatAztec", "message": "M1EXAMPLE/ADA         EXMPL01 FRALISXA 0123 324Y014C0001 100", "messageEncoding": "iso-8859-1"}
  ],
  "semantics": {
    "airlineCode": "XA",
    "flightCode": "XA123",
    "flightNumber": 123,
    "departureAirportCode": "FRA",
    "destinationAirportCode": "LIS",
    "seats": [{"seatRow": "14", "seatNumber": "C"}],
    "confirmationNumber": "EXMPL01",
    "passengerName": {"givenName": "Ada", "familyName": "Example"},
    "originalDepartureDate": "2026-11-20T08:00+01:00",
    "currentDepartureDate": "2026-11-20T08:15+01:00"
  }
}"#
    .into()
}

pub fn event_json() -> String {
    r#"{
  "formatVersion": 1,
  "passTypeIdentifier": "pass.com.example.tickets",
  "serialNumber": "EV-77",
  "teamIdentifier": "EXAMPLE02",
  "organizationName": "Example Hall",
  "description": "Concert ticket",
  "relevantDate": "2026-12-05T19:30",
  "eventTicket": {"primaryFields": [{"key": "event", "label": "Event", "value": "Winter concert"}]},
  "barcodes": [{"format": "PKBarcodeFormatQR", "message": "EV-77-TICKET", "messageEncoding": "utf-8"}]
}"#
    .into()
}

pub fn edit(json: &str, change: impl FnOnce(&mut serde_json::Map<String, Value>)) -> String {
    let mut value: Value = serde_json::from_str(json).unwrap();
    change(value.as_object_mut().unwrap());
    value.to_string()
}

pub fn without(json: &str, key: &str) -> String {
    edit(json, |map| {
        map.remove(key);
    })
}

pub fn attachment(ordinal: u16, content: Vec<u8>) -> AttachmentMeta {
    AttachmentMeta {
        ordinal,
        filename: Some("pass.pkpass".into()),
        content_type: PKPASS.into(),
        size: content.len() as u64,
        content: Some(Bytes::from(content)),
    }
}

pub fn input(contents: Vec<Vec<u8>>) -> InsightInput {
    InsightInput {
        subject: Some("Your pass".into()),
        attachments: contents
            .into_iter()
            .enumerate()
            .map(|(ordinal, content)| attachment(ordinal as u16, content))
            .collect(),
        ..InsightInput::default()
    }
}

/// A pass whose `pass.json` declares 100 bytes and inflates to more than 20 MiB.
pub fn lying_bomb() -> Vec<u8> {
    let mut json = boarding_json().into_bytes();
    json.pop();
    json.extend_from_slice(b",\"pad\":\"");
    json.extend(std::iter::repeat_n(b' ', 20 * 1024 * 1024));
    json.extend_from_slice(b"\"}");
    let honest = pkpass(&[("pass.json", &json)]);
    patch(honest, 22, 24, &100u32.to_le_bytes())
}

/// A pass whose `strip.png` honestly declares 11 MiB of zeros next to a valid `pass.json`.
pub fn honest_bomb() -> Vec<u8> {
    let zeros = vec![0u8; 11 * 1024 * 1024];
    pkpass(&[
        ("pass.json", boarding_json().as_bytes()),
        ("strip.png", &zeros),
    ])
}

/// The named fixture cases, each with whether it gives a pass.
pub fn fixtures() -> Vec<(&'static str, Vec<u8>, bool)> {
    let boarding = boarding_json();
    let many: Vec<(String, Vec<u8>)> = (0..64)
        .map(|i| (format!("icon{i}.png"), vec![1u8; 8]))
        .collect();
    let with_many = |extra: usize| {
        let mut entries: Vec<(&str, &[u8])> = vec![("pass.json", boarding.as_bytes())];
        entries.extend(
            many.iter()
                .take(63 + extra)
                .map(|(name, data)| (name.as_str(), data.as_slice())),
        );
        pkpass(&entries)
    };
    let stored = zip(
        &[("pass.json", boarding.as_bytes())],
        CompressionMethod::Stored,
    );
    let mut bom = b"\xEF\xBB\xBF".to_vec();
    bom.extend_from_slice(boarding.as_bytes());
    let oversized = zip(
        &[
            ("pass.json", boarding.as_bytes()),
            ("noise.bin", &vec![7u8; 2 * 1024 * 1024]),
        ],
        CompressionMethod::Stored,
    );
    let big_json = edit(&boarding, |map| {
        map.insert("pad".into(), Value::String(" ".repeat(600 * 1024)));
    });

    vec![
        (
            "pkpass-boarding-air",
            pkpass(&[("pass.json", boarding.as_bytes())]),
            true,
        ),
        (
            "pkpass-event-ticket",
            pkpass(&[("pass.json", event_json().as_bytes())]),
            true,
        ),
        ("pkpass-stored", stored.clone(), true),
        ("pkpass-bom", pkpass(&[("pass.json", &bom)]), true),
        ("pkpass-64-entries", with_many(0), true),
        (
            "pkpass-boarding-no-semantics",
            pkpass(&[("pass.json", without(&boarding, "semantics").as_bytes())]),
            true,
        ),
        ("pkpass-zip-bomb", lying_bomb(), false),
        ("pkpass-zip-bomb-honest", honest_bomb(), false),
        (
            "pkpass-pass-json-too-large",
            pkpass(&[("pass.json", big_json.as_bytes())]),
            false,
        ),
        (
            "pkpass-dotdot-entry",
            pkpass(&[
                ("pass.json", boarding.as_bytes()),
                ("en.lproj/../../icon.png", b"x"),
            ]),
            false,
        ),
        (
            "pkpass-absolute-entry",
            pkpass(&[("pass.json", boarding.as_bytes()), ("/icon.png", b"x")]),
            false,
        ),
        ("pkpass-too-many-entries", with_many(1), false),
        (
            "pkpass-missing-description",
            pkpass(&[("pass.json", without(&boarding, "description").as_bytes())]),
            false,
        ),
        (
            "pkpass-missing-organization",
            pkpass(&[(
                "pass.json",
                without(&boarding, "organizationName").as_bytes(),
            )]),
            false,
        ),
        (
            "pkpass-nested-pass-json",
            pkpass(&[("en.lproj/pass.json", boarding.as_bytes())]),
            false,
        ),
        (
            "pkpass-not-zip",
            b"PK\x03\x04 this is not a zip archive".to_vec(),
            false,
        ),
        ("pkpass-empty", Vec::new(), false),
        (
            "pkpass-encrypted",
            patch(stored.clone(), 6, 8, &1u16.to_le_bytes()),
            false,
        ),
        (
            "pkpass-bzip2",
            patch(stored, 8, 10, &12u16.to_le_bytes()),
            false,
        ),
        ("pkpass-content-over-cap", oversized, false),
        (
            "pkpass-invalid-json",
            pkpass(&[("pass.json", b"{\"organizationName\":")]),
            false,
        ),
    ]
}

/// Two root entries named `pass.json`, renamed after writing since the writer refuses duplicates.
pub fn duplicate_pass_json(json: &str) -> Vec<u8> {
    let bytes = zip(
        &[
            ("pass.json", json.as_bytes()),
            ("pass.jsoX", json.as_bytes()),
        ],
        CompressionMethod::Stored,
    );
    let mut out = Vec::with_capacity(bytes.len());
    let mut rest = bytes.as_slice();
    while !rest.is_empty() {
        if rest.starts_with(b"pass.jsoX") {
            out.extend_from_slice(b"pass.json");
            rest = &rest[9..];
        } else {
            out.push(rest[0]);
            rest = &rest[1..];
        }
    }
    out
}
