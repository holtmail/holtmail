use holtmail_store::Person;

/// Escapes a vCard 3.0 text value (RFC 2426 section 4).
fn text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ',' => out.push_str("\\,"),
            ';' => out.push_str("\\;"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// Folds a content line at 75 octets without splitting a character.
pub(crate) fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut width = 0;
    for c in line.chars() {
        let len = c.len_utf8();
        if width + len > 75 {
            out.push_str("\r\n ");
            width = 1;
        }
        out.push(c);
        width += len;
    }
    out.push_str("\r\n");
    out
}

/// A person of the merged "People" book as a vCard 3.0, which every client reads: `UID` is the
/// person's id, `FN` the name shown (else the first address or phone), then every address and
/// phone number. Deterministic, so the same person always gives the same bytes.
pub(crate) fn vcard(person: &Person) -> Vec<u8> {
    let name = person
        .name
        .clone()
        .or_else(|| person.email.clone())
        .or_else(|| person.phone.clone())
        .unwrap_or_default();
    let mut out = String::from("BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//Holtmail//People//EN\r\n");
    out.push_str(&fold(&format!("UID:{}", person.id)));
    out.push_str(&fold(&format!("FN:{}", text(&name))));
    out.push_str(&fold(&format!("N:{};;;;", text(&name))));
    let mut emails: Vec<&String> = Vec::new();
    if let Some(first) = &person.email {
        emails.push(first);
    }
    emails.extend(
        person
            .emails
            .iter()
            .filter(|e| Some(*e) != person.email.as_ref()),
    );
    for email in emails {
        out.push_str(&fold(&format!("EMAIL;TYPE=INTERNET:{}", text(email))));
    }
    let mut phones: Vec<&String> = Vec::new();
    if let Some(first) = &person.phone {
        phones.push(first);
    }
    phones.extend(
        person
            .phones
            .iter()
            .filter(|p| Some(*p) != person.phone.as_ref()),
    );
    for phone in phones {
        out.push_str(&fold(&format!("TEL;TYPE=VOICE:{}", text(phone))));
    }
    out.push_str("END:VCARD\r\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use holtmail_core::{ArchiveId, PersonId};

    #[test]
    fn writes_a_readable_card() {
        let person = Person {
            id: PersonId::from_hash([3; 16]),
            archive_id: ArchiveId::new(),
            name: Some("Bea, Example".into()),
            email: Some("bea@example.org".into()),
            phone: None,
            emails: vec!["b@example.com".into(), "bea@example.org".into()],
            mail_emails: Vec::new(),
            phones: vec!["+15550100".into()],
            cards: Vec::new(),
            links: Vec::new(),
            mail: None,
            differences: false,
            removed: false,
        };
        let card = String::from_utf8(vcard(&person)).unwrap();
        assert!(card.contains("FN:Bea\\, Example\r\n"));
        assert!(card.find("bea@example.org").unwrap() < card.find("b@example.com").unwrap());
        assert!(card.contains("TEL;TYPE=VOICE:+15550100\r\n"));
        match holtmail_pim::normalize(holtmail_pim::ItemKind::Contact, card.as_bytes()) {
            holtmail_pim::Normalized::Contact(_) => {}
            other => panic!("{other:?}"),
        }
        assert_eq!(vcard(&person), card.into_bytes());
    }

    #[test]
    fn folds_at_75_octets() {
        let long = format!("NOTE:{}", "ä".repeat(60));
        let folded = fold(&long);
        assert!(folded.split("\r\n").all(|l| l.len() <= 75));
        assert_eq!(folded.replace("\r\n ", ""), format!("{long}\r\n"));
    }
}
