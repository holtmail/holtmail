#[derive(Debug, Default)]
pub(crate) struct Style(Vec<(String, String)>);

impl Style {
    pub(crate) fn parse(text: &str) -> Self {
        Self(
            text.split(';')
                .filter_map(|declaration| {
                    let (property, value) = declaration.split_once(':')?;
                    let property = property.trim().to_ascii_lowercase();
                    let value = value.trim().to_ascii_lowercase();
                    let value = value
                        .strip_suffix("!important")
                        .map_or(value.as_str(), str::trim_end)
                        .to_owned();
                    (!property.is_empty()).then_some((property, value))
                })
                .collect(),
        )
    }

    fn get(&self, property: &str) -> Option<&str> {
        self.0
            .iter()
            .rev()
            .find(|(name, _)| name == property)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn hides(&self) -> bool {
        self.get("display") == Some("none")
            || matches!(self.get("visibility"), Some("hidden" | "collapse"))
            || self.get("font-size").is_some_and(is_zero)
            || self.get("opacity").is_some_and(is_zero)
            || (self.get("max-height").is_some_and(is_zero)
                && (self.get("overflow") == Some("hidden")
                    || self.get("overflow-y") == Some("hidden")))
    }

    pub(crate) fn emphasis(&self) -> bool {
        let bold = self.get("font-weight").is_some_and(|value| {
            matches!(value, "bold" | "bolder") || number(value).is_some_and(|(n, _)| n >= 600.0)
        });
        let spaced = self
            .get("letter-spacing")
            .is_some_and(|value| number(value).is_some_and(|(n, _)| n > 0.0));
        bold || spaced || self.get("font-size").is_some_and(large_font)
    }

    pub(crate) fn button(&self) -> bool {
        self.0
            .iter()
            .any(|(property, value)| match property.as_str() {
                "background" | "background-color" | "background-image" => !matches!(
                    value.as_str(),
                    "" | "none" | "transparent" | "inherit" | "initial" | "unset"
                ),
                p if p.starts_with("padding") => value
                    .split_whitespace()
                    .any(|part| number(part).is_some_and(|(n, _)| n > 0.0)),
                _ => false,
            })
    }
}

fn number(value: &str) -> Option<(f64, &str)> {
    let value = value.trim();
    let end = value
        .find(|c: char| !(c.is_ascii_digit() || matches!(c, '.' | '-' | '+')))
        .unwrap_or(value.len());
    let (digits, unit) = value.split_at_checked(end)?;
    let n = digits.parse::<f64>().ok().filter(|n| n.is_finite())?;
    Some((n, unit.trim()))
}

fn is_zero(value: &str) -> bool {
    number(value).is_some_and(|(n, _)| n == 0.0)
}

fn large_font(value: &str) -> bool {
    if matches!(
        value,
        "large" | "x-large" | "xx-large" | "xxx-large" | "larger"
    ) {
        return true;
    }
    match number(value) {
        Some((n, "px" | "")) => n >= 20.0,
        Some((n, "pt")) => n >= 15.0,
        Some((n, "em" | "rem")) => n >= 1.25,
        Some((n, "%")) => n >= 125.0,
        _ => false,
    }
}

pub(crate) fn large_font_attribute(size: &str) -> bool {
    let size = size.trim();
    match size.strip_prefix('+') {
        Some(step) => step.parse::<u8>().is_ok_and(|n| n >= 2),
        None => size.parse::<u8>().is_ok_and(|n| n >= 5),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiding_rules() {
        for hidden in [
            "display:none",
            "DISPLAY: None !important",
            "visibility:hidden",
            "font-size:0",
            "font-size: 0px",
            "opacity:0",
            "opacity: 0.0",
            "max-height:0;overflow:hidden",
            "max-height:0px; overflow-y:hidden",
        ] {
            assert!(Style::parse(hidden).hides(), "{hidden}");
        }
        for shown in [
            "display:block",
            "max-height:0",
            "font-size:10px",
            "opacity:0.5",
            "display:none; display:block",
            "color:red",
            "",
        ] {
            assert!(!Style::parse(shown).hides(), "{shown}");
        }
    }

    #[test]
    fn emphasis_rules() {
        for strong in [
            "font-weight:bold",
            "font-weight:700",
            "letter-spacing:4px",
            "font-size:24px",
            "font-size:18pt",
            "font-size:1.5em",
            "font-size:x-large",
        ] {
            assert!(Style::parse(strong).emphasis(), "{strong}");
        }
        for plain in [
            "font-weight:normal",
            "letter-spacing:0",
            "letter-spacing:normal",
            "font-size:14px",
            "font-size:1em",
        ] {
            assert!(!Style::parse(plain).emphasis(), "{plain}");
        }
        assert!(large_font_attribute("6"));
        assert!(large_font_attribute("+2"));
        assert!(!large_font_attribute("3"));
        assert!(!large_font_attribute("+1"));
    }

    #[test]
    fn button_rules() {
        assert!(Style::parse("background-color:#1a73e8").button());
        assert!(Style::parse("padding:12px 24px").button());
        assert!(!Style::parse("padding:0").button());
        assert!(!Style::parse("background:transparent").button());
        assert!(!Style::parse("color:#fff").button());
    }
}
