use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use holtmail_insight::{CarrierId, Currency, Url};
use serde::Deserialize;

const CURRENCIES: &str = include_str!("../data/currencies.toml");
const CARRIERS: &str = include_str!("../data/carriers.toml");

/// The version of the reading and mapping code; the data hash is appended to it.
pub const ALGORITHM: u32 = 1;

const MAX_SLUG_CHARS: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrencyFile {
    currencies: BTreeMap<String, u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierFile {
    carriers: BTreeMap<String, CarrierEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CarrierEntry {
    names: Vec<String>,
    hosts: Vec<String>,
}

pub(crate) struct Data {
    currencies: BTreeMap<String, Currency>,
    names: BTreeMap<String, CarrierId>,
    hosts: Vec<(String, CarrierId)>,
    pub version: String,
}

pub(crate) static DATA: LazyLock<Option<Arc<Data>>> =
    LazyLock::new(|| Data::parse(CURRENCIES, CARRIERS).ok().map(Arc::new));

impl Data {
    pub(crate) fn parse(currencies: &str, carriers: &str) -> Result<Self, String> {
        let file: CurrencyFile =
            toml::from_str(currencies).map_err(|_| "currencies.toml does not parse".to_owned())?;
        let mut table = BTreeMap::new();
        for (code, exponent) in file.currencies {
            let currency = Currency::new(&code, exponent)
                .map_err(|_| format!("currency {code} is invalid"))?;
            table.insert(code, currency);
        }
        let file: CarrierFile =
            toml::from_str(carriers).map_err(|_| "carriers.toml does not parse".to_owned())?;
        let mut names = BTreeMap::new();
        let mut hosts = Vec::new();
        for (id, entry) in file.carriers {
            let carrier = CarrierId::new(&id).map_err(|_| format!("carrier id {id} is invalid"))?;
            for name in entry.names {
                let key = name_key(&name);
                if key.is_empty() || names.insert(key, carrier.clone()).is_some() {
                    return Err(format!("carrier name of {id} is empty or repeated"));
                }
            }
            for host in entry.hosts {
                if host.is_empty() || host != host.to_ascii_lowercase() {
                    return Err(format!("carrier host of {id} is not lowercase"));
                }
                hosts.push((host, carrier.clone()));
            }
        }
        let mut hasher = blake3::Hasher::new();
        for source in [currencies, carriers] {
            hasher.update(&(source.len() as u64).to_le_bytes());
            hasher.update(source.as_bytes());
        }
        let hash = hasher.finalize().to_hex();
        let short = hash.as_str().get(..12).unwrap_or_default();
        Ok(Self {
            currencies: table,
            names,
            hosts,
            version: format!("{ALGORITHM}+{short}"),
        })
    }

    /// The currency of an ISO 4217 code in any letter case, when the table knows it.
    pub(crate) fn currency(&self, code: &str) -> Option<Currency> {
        self.currencies
            .get(&code.trim().to_ascii_uppercase())
            .cloned()
    }

    /// The carrier for a name or URL from the message: the table's id when a name or host
    /// matches, else the slug of the name.
    pub(crate) fn carrier(&self, names: &[&str], urls: &[&str]) -> Option<CarrierId> {
        for name in names {
            if let Some(id) = self.names.get(&name_key(name)) {
                return Some(id.clone());
            }
        }
        for url in urls {
            let Ok(url) = Url::parse(url.trim()) else {
                continue;
            };
            let Some(host) = url.host() else {
                continue;
            };
            for (hint, id) in &self.hosts {
                let under = host
                    .strip_suffix(hint.as_str())
                    .is_some_and(|rest| rest.is_empty() || rest.ends_with('.'));
                if under {
                    return Some(id.clone());
                }
            }
        }
        names
            .iter()
            .find_map(|name| CarrierId::new(&slug(name)).ok())
    }
}

fn name_key(name: &str) -> String {
    let lower = name.to_lowercase();
    let mut out = String::new();
    for word in lower.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
        if out.len() >= MAX_SLUG_CHARS {
            break;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> Arc<Data> {
        DATA.clone().expect("embedded data parses")
    }

    #[test]
    fn embedded_tables_parse() {
        let data = data();
        assert!(data.version.starts_with("1+"));
        assert_eq!(data.version.len(), 14);
        assert_eq!(data.currency("JPY").map(|c| c.exponent()), Some(0));
        assert_eq!(data.currency("usd").map(|c| c.exponent()), Some(2));
        assert_eq!(data.currency("KWD").map(|c| c.exponent()), Some(3));
        assert_eq!(data.currency("BHD").map(|c| c.exponent()), Some(3));
        assert!(data.currency("XAU").is_none());
        assert!(data.currency("ZZZ").is_none());
    }

    #[test]
    fn carriers_match_by_name_host_or_slug() {
        let data = data();
        let id = |names: &[&str], urls: &[&str]| {
            data.carrier(names, urls).map(|c| c.as_str().to_owned())
        };
        assert_eq!(id(&["United Parcel Service"], &[]).as_deref(), Some("ups"));
        assert_eq!(id(&["FedEx"], &[]).as_deref(), Some("fedex"));
        assert_eq!(id(&["Почта России"], &[]).as_deref(), Some("russian_post"));
        assert_eq!(
            id(&[], &["https://www.dhl.com/en/express.html"]).as_deref(),
            Some("dhl_express")
        );
        assert_eq!(id(&[], &["https://notups.com/"]), None);
        assert_eq!(
            id(&["Example Parcel Co."], &[]).as_deref(),
            Some("example_parcel_co")
        );
        assert_eq!(id(&["Курьер"], &[]), None);
        assert_eq!(id(&[""], &[]), None);
        let long = id(&[&"x".repeat(100)], &[]).unwrap();
        assert_eq!(long.len(), MAX_SLUG_CHARS);
    }

    #[test]
    fn bad_tables_are_refused() {
        assert!(Data::parse("[currencies]\nusd = 2\n", CARRIERS).is_err());
        assert!(Data::parse("[currencies]\nUSD = 9\n", CARRIERS).is_err());
        assert!(
            Data::parse(
                CURRENCIES,
                "[carriers.UPS]\nnames = [\"ups\"]\nhosts = []\n"
            )
            .is_err()
        );
        assert!(
            Data::parse(
                CURRENCIES,
                "[carriers.a]\nnames = [\"x\"]\nhosts = []\n[carriers.b]\nnames = [\"X\"]\nhosts = []\n"
            )
            .is_err()
        );
    }
}
