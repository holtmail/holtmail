use frostem::{Algorithm, Stemmer};
use unicode_normalization::char::{decompose_compatible, is_combining_mark};

const MIN_PREFIX: usize = 3;
const MIN_STEM: usize = 4;

pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|run| !run.is_empty())
        .map(|run| {
            let mut out = String::with_capacity(run.len());
            for c in run.chars().flat_map(char::to_lowercase) {
                fold(c, &mut out);
            }
            out
        })
        .collect()
}

fn fold(c: char, out: &mut String) {
    if c.is_ascii() {
        out.push(c);
        return;
    }
    let mut parts = String::new();
    let mut plain = true;
    decompose_compatible(c, |d| {
        if is_combining_mark(d) {
            return;
        }
        plain &= d.is_ascii();
        parts.push(d);
    });
    if plain && !parts.is_empty() {
        out.push_str(&parts);
        return;
    }
    out.push_str(match c {
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'ø' => "o",
        'ł' => "l",
        'đ' | 'ð' => "d",
        'þ' => "th",
        'ı' => "i",
        'ħ' => "h",
        'ŧ' => "t",
        'ŋ' => "n",
        _ => {
            out.push(c);
            return;
        }
    });
}

#[derive(Debug, Clone)]
pub(crate) struct Word {
    text: String,
    root: Option<String>,
}

impl Word {
    pub(crate) fn new(text: String, stem: bool) -> Self {
        let root = (stem && text.chars().count() >= MIN_PREFIX)
            .then(|| {
                let english = Stemmer::new(Algorithm::English).stem(&text).into_owned();
                Stemmer::new(Algorithm::Russian).stem(&english).into_owned()
            })
            .filter(|root| *root != text && root.chars().count() >= MIN_STEM);
        Self { text, root }
    }

    pub(crate) fn finds(&self, token: &str) -> bool {
        if self.text.chars().count() < MIN_PREFIX {
            return token == self.text;
        }
        token.starts_with(&self.text) || self.root.as_ref().is_some_and(|r| token.starts_with(r))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lowercases_and_folds() {
        assert_eq!(
            words("Zoë's café, ada@example.com STRAẞE Søren Łódź ﬁle"),
            [
                "zoe", "s", "cafe", "ada", "example", "com", "strasse", "soren", "lodz", "file"
            ]
        );
        assert_eq!(words("Мой ёж, Привет!"), ["мой", "ёж", "привет"]);
        assert!(words(" -- ").is_empty());
    }

    #[test]
    fn finds_prefixes_and_other_forms() {
        let word = |w: &str, stem| Word::new(w.into(), stem);
        assert!(word("quart", false).finds("quarterly"));
        assert!(!word("lu", false).finds("lunch"));
        assert!(word("lu", false).finds("lu"));
        assert!(word("repairing", true).finds("repairs"));
        assert!(!word("repairing", false).finds("repairs"));
        assert!(word("договорам", true).finds("договор"));
    }
}
