use serde_json::{Map, Value};

use crate::error::MethodError;

/// Evaluates an RFC 6901 pointer with the JMAP `*` extension (RFC 8620 §3.7): `*` on an array
/// maps the rest of the pointer over every item and flattens arrays one level.
pub(crate) fn pointer(value: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return Some(value.clone());
    }
    let rest = path.strip_prefix('/')?;
    let tokens: Vec<String> = rest
        .split('/')
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect();
    walk(value, &tokens)
}

fn walk(value: &Value, tokens: &[String]) -> Option<Value> {
    let Some((first, rest)) = tokens.split_first() else {
        return Some(value.clone());
    };
    match value {
        Value::Array(items) if first == "*" => {
            let mut out = Vec::new();
            for item in items {
                match walk(item, rest)? {
                    Value::Array(inner) => out.extend(inner),
                    other => out.push(other),
                }
            }
            Some(Value::Array(out))
        }
        Value::Array(items) => walk(items.get(first.parse::<usize>().ok()?)?, rest),
        Value::Object(map) => walk(map.get(first)?, rest),
        _ => None,
    }
}

/// The arguments of one method call.
pub(crate) struct Args<'a>(pub &'a Map<String, Value>);

impl Args<'_> {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    pub fn string(&self, key: &str) -> Result<Option<&str>, MethodError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s)),
            Some(_) => Err(MethodError::invalid(format!("{key} must be a string"))),
        }
    }

    pub fn required_string(&self, key: &str) -> Result<&str, MethodError> {
        self.string(key)?
            .ok_or_else(|| MethodError::invalid(format!("{key} is required")))
    }

    pub fn bool(&self, key: &str, default: bool) -> Result<bool, MethodError> {
        match self.get(key) {
            None => Ok(default),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(MethodError::invalid(format!("{key} must be true or false"))),
        }
    }

    pub fn int(&self, key: &str) -> Result<Option<i64>, MethodError> {
        match self.get(key) {
            None => Ok(None),
            Some(v) => v
                .as_i64()
                .map(Some)
                .ok_or_else(|| MethodError::invalid(format!("{key} must be an integer"))),
        }
    }

    pub fn unsigned(&self, key: &str) -> Result<Option<u64>, MethodError> {
        match self.int(key)? {
            Some(n) if n < 0 => Err(MethodError::invalid(format!("{key} must not be negative"))),
            other => Ok(other.map(|n| n as u64)),
        }
    }

    pub fn strings(&self, key: &str) -> Result<Option<Vec<String>>, MethodError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Array(items)) => items
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| MethodError::invalid(format!("{key} must hold strings")))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(Some),
            Some(_) => Err(MethodError::invalid(format!("{key} must be a list"))),
        }
    }

    pub fn object(&self, key: &str) -> Result<Option<&Map<String, Value>>, MethodError> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Object(map)) => Ok(Some(map)),
            Some(_) => Err(MethodError::invalid(format!("{key} must be an object"))),
        }
    }
}

/// Keeps only the asked properties of an object; `id` is always kept.
pub(crate) fn only(mut object: Map<String, Value>, properties: Option<&[String]>) -> Value {
    if let Some(properties) = properties {
        object.retain(|k, _| k == "id" || properties.iter().any(|p| p == k));
    }
    Value::Object(object)
}

/// Applies a `/set` update to a set-like property (`keywords`, `mailboxIds`): either the whole
/// property, or patches `property/key` with `true` or `null`. Returns the new set, or `None`
/// when the update does not touch the property. Keys compare as `fold` makes them.
pub(crate) fn patch_set(
    current: &[String],
    update: &Map<String, Value>,
    property: &str,
    fold: impl Fn(&str) -> String,
) -> Result<Option<Vec<String>>, String> {
    let mut next: Vec<String> = current.iter().map(|k| fold(k)).collect();
    let mut touched = false;
    if let Some(whole) = update.get(property) {
        touched = true;
        next.clear();
        let map = whole
            .as_object()
            .ok_or_else(|| format!("{property} must be an object"))?;
        for (key, value) in map {
            if value != &Value::Bool(true) {
                return Err(format!("{property} values must be true"));
            }
            next.push(fold(key));
        }
    }
    let prefix = format!("{property}/");
    for (key, value) in update {
        let Some(item) = key.strip_prefix(&prefix) else {
            continue;
        };
        if update.contains_key(property) {
            return Err(format!("{property} is set whole and patched at once"));
        }
        touched = true;
        let item = fold(&item.replace("~1", "/").replace("~0", "~"));
        match value {
            Value::Bool(true) => {
                if !next.contains(&item) {
                    next.push(item);
                }
            }
            Value::Null | Value::Bool(false) => next.retain(|k| k != &item),
            _ => return Err(format!("{key} must be true or null")),
        }
    }
    next.sort();
    next.dedup();
    Ok(touched.then_some(next))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn pointers_map_over_arrays() {
        let value = json!({"list": [{"id": "a", "ids": ["x", "y"]}, {"id": "b", "ids": ["z"]}]});
        assert_eq!(pointer(&value, "/list/*/id"), Some(json!(["a", "b"])));
        assert_eq!(pointer(&value, "/list/*/ids"), Some(json!(["x", "y", "z"])));
        assert_eq!(pointer(&value, "/list/1/id"), Some(json!("b")));
        assert_eq!(pointer(&value, "/nope"), None);
        assert_eq!(pointer(&json!({"a/b": 1}), "/a~1b"), Some(json!(1)));
    }

    #[test]
    fn set_patches() {
        let current = vec!["$seen".to_string()];
        let lower = |s: &str| s.to_ascii_lowercase();
        let update = json!({"keywords/$Flagged": true, "keywords/$seen": null});
        assert_eq!(
            patch_set(&current, update.as_object().unwrap(), "keywords", lower),
            Ok(Some(vec!["$flagged".into()]))
        );
        let update = json!({"keywords": {"$draft": true}});
        assert_eq!(
            patch_set(&current, update.as_object().unwrap(), "keywords", lower),
            Ok(Some(vec!["$draft".into()]))
        );
        let update = json!({"subject": "x"});
        assert_eq!(
            patch_set(&current, update.as_object().unwrap(), "keywords", lower),
            Ok(None)
        );
        let update = json!({"keywords": {"$a": true}, "keywords/$b": true});
        assert!(patch_set(&current, update.as_object().unwrap(), "keywords", lower).is_err());
    }
}
