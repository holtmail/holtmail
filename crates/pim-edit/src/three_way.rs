use holtmail_pim::{ItemKind, Normalized, normalize};
use serde::{Deserialize, Serialize};

/// One field of a conflict view: its values in the base, in the change and upstream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldDiff {
    /// The field as `holtmail_pim::diff` names it, such as `start`, `attendee` or
    /// `instance 2026-12-01T10:00:00 Europe/Berlin summary`.
    pub field: String,
    /// Its values in the base version; empty when it had none.
    pub base: Vec<String>,
    /// Its values in the change.
    pub mine: Vec<String>,
    /// Its values in the newer upstream version.
    pub theirs: Vec<String>,
    /// The change differs from the base here.
    pub mine_changed: bool,
    /// Upstream differs from the base here.
    pub theirs_changed: bool,
    /// Both differ from the base, and from each other.
    pub conflict: bool,
}

const SKIPPED: &[&str] = &["sequence", "rev", "uid"];

fn fields(kind: ItemKind, raw: Option<&[u8]>) -> Vec<(String, String)> {
    match raw.map(|r| normalize(kind, r)) {
        Some(n @ (Normalized::Contact(_) | Normalized::Calendar(_))) => n.fields(),
        _ => Vec::new(),
    }
}

fn values(fields: &[(String, String)], name: &str) -> Vec<String> {
    fields
        .iter()
        .filter(|(n, _)| n == name)
        .map(|(_, v)| v.clone())
        .collect()
}

fn same(a: &[String], b: &[String]) -> bool {
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort();
    b.sort();
    a == b
}

/// The fields where the change (`mine`) or upstream (`theirs`) differs from `base`, from the
/// normalized models of `holtmail-pim`, in the order the fields first appear. `None` stands for
/// a side that has no item (a create's base, a deleted upstream). Several values of one field
/// compare as a set. `SEQUENCE`, `REV` and `UID` are left out, as are alarms and client
/// properties, which never conflict.
pub fn field_diff(
    kind: ItemKind,
    base: Option<&[u8]>,
    mine: Option<&[u8]>,
    theirs: Option<&[u8]>,
) -> Vec<FieldDiff> {
    let base = fields(kind, base);
    let mine = fields(kind, mine);
    let theirs = fields(kind, theirs);
    let mut names: Vec<&str> = Vec::new();
    for (name, _) in base.iter().chain(mine.iter()).chain(theirs.iter()) {
        let last = name.rsplit(' ').next().unwrap_or(name);
        if !names.contains(&name.as_str()) && !SKIPPED.contains(&last) {
            names.push(name);
        }
    }
    names
        .into_iter()
        .filter_map(|name| {
            let b = values(&base, name);
            let m = values(&mine, name);
            let t = values(&theirs, name);
            let mine_changed = !same(&b, &m);
            let theirs_changed = !same(&b, &t);
            (mine_changed || theirs_changed).then(|| FieldDiff {
                field: name.to_owned(),
                conflict: mine_changed && theirs_changed && !same(&m, &t),
                base: b,
                mine: m,
                theirs: t,
                mine_changed,
                theirs_changed,
            })
        })
        .collect()
}
