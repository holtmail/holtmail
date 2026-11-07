use std::collections::HashMap;

use holtmail_insight::limits::{MAX_MICRODATA_DEPTH, MAX_MICRODATA_PROPERTIES, MAX_URL_BYTES};
use holtmail_insight::{MicrodataItem, MicrodataProperty, MicrodataValue, truncate_display};
use html5ever::tokenizer::Tag;

use crate::scan::collapse;

pub(crate) const MAX_ITEMS: usize = MAX_MICRODATA_PROPERTIES;

pub(crate) const MAX_VISITS: usize = 16 * MAX_MICRODATA_PROPERTIES;

pub(crate) type Pos = (usize, bool);

pub(crate) fn below(at: Pos, forward: Option<usize>) -> bool {
    forward.is_some_and(|from| at.0 > from || (at.0 == from && at.1))
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Micro {
    item: Option<u32>,
    pub(crate) scope: Option<u32>,
    collect: Option<(u32, u32)>,
    range: Option<u32>,
}

impl Micro {
    pub(crate) fn scoped(mut self, parent: Option<u32>) -> Micro {
        self.scope = self.item.or(parent);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Leaf {
    Element,
    Void,
    Raw,
    Flat,
    Root,
}

#[derive(Debug, Default)]
pub(crate) struct Attrs<'t> {
    itemscope: bool,
    itemprop: Option<&'t str>,
    itemtype: Option<&'t str>,
    itemid: Option<&'t str>,
    itemref: Option<&'t str>,
    id: Option<&'t str>,
}

impl<'t> Attrs<'t> {
    pub(crate) fn of(tag: &'t Tag) -> Option<Attrs<'t>> {
        let mut attrs = Attrs::default();
        let mut any = false;
        for attr in &tag.attrs {
            let value = &*attr.value;
            match &*attr.name.local {
                "itemscope" => attrs.itemscope = true,
                "itemprop" => attrs.itemprop = Some(value),
                "itemtype" => attrs.itemtype = Some(value),
                "itemid" => attrs.itemid = Some(value),
                "itemref" => attrs.itemref = Some(value),
                "id" => attrs.id = Some(value),
                _ => continue,
            }
            any = true;
        }
        any.then_some(attrs)
    }

    pub(crate) fn itemscope(&self) -> bool {
        self.itemscope
    }
}

#[derive(Debug)]
struct Item {
    types: Vec<String>,
    id: Option<String>,
    refs: Option<String>,
    top_level: bool,
    at: Pos,
    depth: usize,
}

#[derive(Debug)]
struct Record {
    name: String,
    owner: Option<u32>,
    value: MicrodataValue,
    start: usize,
}

#[derive(Debug, Clone, Copy)]
struct Range {
    records: (u32, u32),
    items: (u32, u32),
}

#[derive(Debug, Default)]
pub(crate) struct Microdata {
    items: Vec<Item>,
    records: Vec<Record>,
    ranges: Vec<Range>,
    ids: HashMap<String, u32>,
    log: String,
    space: bool,
    collecting: u32,
    pub(crate) truncated: bool,
}

enum Value<'t> {
    Url(&'t str),
    Text(&'t str),
    Content,
    Skip,
}

fn value_of<'t>(tag: &'t Tag, leaf: Leaf) -> Value<'t> {
    let attribute = |name: &str| {
        tag.attrs
            .iter()
            .find(|a| &*a.name.local == name)
            .map(|a| &*a.value)
    };
    let url = match &*tag.name {
        "audio" | "embed" | "iframe" | "img" | "source" | "track" | "video" => Some("src"),
        "a" | "area" | "link" => Some("href"),
        "object" => Some("data"),
        _ => None,
    };
    if let Some(name) = url {
        return Value::Url(attribute(name).unwrap_or_default());
    }
    let text = match &*tag.name {
        "meta" => Some("content"),
        "data" | "meter" => Some("value"),
        _ => None,
    };
    if let Some(name) = text {
        return Value::Text(attribute(name).unwrap_or_default());
    }
    if &*tag.name == "time"
        && let Some(datetime) = attribute("datetime")
    {
        return Value::Text(datetime);
    }
    match leaf {
        Leaf::Element => Value::Content,
        Leaf::Void => Value::Text(""),
        Leaf::Raw | Leaf::Flat | Leaf::Root => Value::Skip,
    }
}

fn index(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl Microdata {
    pub(crate) fn open(
        &mut self,
        tag: &Tag,
        attrs: &Attrs,
        scope: Option<u32>,
        at: Pos,
        leaf: Leaf,
    ) -> Micro {
        let mut micro = Micro::default();
        if leaf == Leaf::Raw && !matches!(value_of(tag, leaf), Value::Url(_)) {
            return micro;
        }
        if leaf != Leaf::Root
            && let Some(id) = attrs.id.filter(|id| !id.is_empty())
        {
            let range = index(self.ranges.len());
            self.ranges.push(Range {
                records: (index(self.records.len()), u32::MAX),
                items: (index(self.items.len()), u32::MAX),
            });
            self.ids.entry(id.to_owned()).or_insert(range);
            micro.range = Some(range);
        }
        let names = if leaf == Leaf::Root {
            None
        } else {
            attrs
                .itemprop
                .filter(|names| names.split_ascii_whitespace().next().is_some())
        };
        if attrs.itemscope && leaf != Leaf::Raw {
            micro.item = self.new_item(attrs, scope, at, names.is_none(), leaf);
        }
        if let Some(names) = names {
            let (value, content) = match micro.item {
                Some(item) => (Some(MicrodataValue::Item(item as usize)), false),
                None => match value_of(tag, leaf) {
                    Value::Url(url) if url.len() <= MAX_URL_BYTES => {
                        (Some(MicrodataValue::Url(url.to_owned())), false)
                    }
                    Value::Url(_) => {
                        self.truncated = true;
                        (None, false)
                    }
                    Value::Text(text) => (
                        Some(MicrodataValue::Text(truncate_display(text.trim()))),
                        false,
                    ),
                    Value::Content => (Some(MicrodataValue::Text(String::new())), true),
                    Value::Skip => {
                        self.truncated |= leaf == Leaf::Flat;
                        (None, false)
                    }
                },
            };
            if let Some(value) = value {
                let first = self.records.len();
                for name in names.split_ascii_whitespace() {
                    if self.records.len() >= MAX_MICRODATA_PROPERTIES {
                        self.truncated = true;
                        break;
                    }
                    self.records.push(Record {
                        name: name.to_owned(),
                        owner: scope,
                        value: value.clone(),
                        start: self.log.len(),
                    });
                }
                let count = self.records.len() - first;
                if content && count > 0 {
                    micro.collect = Some((index(first), index(count)));
                    self.collecting += 1;
                }
            }
        }
        if leaf != Leaf::Element {
            self.close_range(micro.range);
            micro.range = None;
        }
        micro
    }

    fn new_item(
        &mut self,
        attrs: &Attrs,
        scope: Option<u32>,
        at: Pos,
        top_level: bool,
        leaf: Leaf,
    ) -> Option<u32> {
        let depth = scope
            .and_then(|s| self.items.get(s as usize))
            .map_or(1, |parent| parent.depth + 1);
        if leaf == Leaf::Flat || depth > MAX_MICRODATA_DEPTH || self.items.len() >= MAX_ITEMS {
            self.truncated = true;
            return None;
        }
        let item = index(self.items.len());
        self.items.push(Item {
            types: attrs
                .itemtype
                .unwrap_or_default()
                .split_ascii_whitespace()
                .map(str::to_owned)
                .collect(),
            id: attrs.itemid.map(str::to_owned),
            refs: attrs
                .itemref
                .filter(|refs| refs.split_ascii_whitespace().next().is_some())
                .map(str::to_owned),
            top_level,
            at,
            depth,
        });
        Some(item)
    }

    fn close_range(&mut self, range: Option<u32>) {
        let records = index(self.records.len());
        let items = index(self.items.len());
        if let Some(range) = range.and_then(|r| self.ranges.get_mut(r as usize)) {
            range.records.1 = records;
            range.items.1 = items;
        }
    }

    pub(crate) fn close(&mut self, micro: Micro) {
        self.close_range(micro.range);
        let Some((first, count)) = micro.collect else {
            return;
        };
        let first = first as usize;
        let start = self.records.get(first).map_or(0, |r| r.start);
        let text = truncate_display(self.log.get(start..).unwrap_or_default().trim());
        for record in self.records.iter_mut().skip(first).take(count as usize) {
            record.value = MicrodataValue::Text(text.clone());
        }
        self.collecting = self.collecting.saturating_sub(1);
        if self.collecting == 0 {
            self.log.clear();
            self.space = false;
        }
    }

    pub(crate) fn text(&mut self, text: &str) {
        if self.collecting > 0 {
            collapse(&mut self.log, &mut self.space, text);
        }
    }

    pub(crate) fn separate(&mut self) {
        if self.collecting > 0 {
            self.space = true;
        }
    }

    pub(crate) fn finish(mut self, forward: Option<usize>) -> (Vec<MicrodataItem>, bool) {
        let mut members: Vec<Vec<u32>> = self.items.iter().map(|_| Vec::new()).collect();
        for (r, record) in self.records.iter().enumerate() {
            if let Some(list) = record.owner.and_then(|o| members.get_mut(o as usize)) {
                list.push(index(r));
            }
        }
        self.expand(&mut members);
        let records = &self.records;
        let microdata = self
            .items
            .into_iter()
            .zip(members)
            .map(|(item, mut list)| {
                list.sort_unstable();
                let mut out = MicrodataItem::new(item.types);
                out.id = item.id;
                out.top_level = item.top_level;
                out.below_forward = below(item.at, forward);
                out.properties = list
                    .iter()
                    .filter_map(|r| records.get(*r as usize))
                    .map(|record| MicrodataProperty {
                        name: record.name.clone(),
                        value: record.value.clone(),
                    })
                    .collect();
                out
            })
            .collect();
        (microdata, self.truncated)
    }

    fn expand(&mut self, members: &mut [Vec<u32>]) {
        let mut count: usize = members.iter().map(Vec::len).sum();
        let mut visits = 0usize;
        let mut stamp = vec![u32::MAX; self.records.len()];
        let mut spans = Vec::new();
        for (i, item) in self.items.iter().enumerate() {
            let Some(refs) = &item.refs else {
                continue;
            };
            let Some(list) = members.get_mut(i) else {
                continue;
            };
            let me = index(i);
            spans.clear();
            spans.extend(
                refs.split_ascii_whitespace()
                    .filter_map(|token| self.ids.get(token).copied()),
            );
            spans.sort_unstable();
            spans.dedup();
            for r in list.iter() {
                if let Some(slot) = stamp.get_mut(*r as usize) {
                    *slot = me;
                }
            }
            for span in &spans {
                let Some(range) = self.ranges.get(*span as usize) else {
                    continue;
                };
                let (from, to) = range.records;
                let (inner_from, inner_to) = range.items;
                for r in from..to.min(index(self.records.len())) {
                    visits += 1;
                    if visits > MAX_VISITS {
                        self.truncated = true;
                        return;
                    }
                    let Some(record) = self.records.get(r as usize) else {
                        break;
                    };
                    if record
                        .owner
                        .is_some_and(|o| o >= inner_from && o < inner_to)
                        || record.value == MicrodataValue::Item(i)
                    {
                        continue;
                    }
                    let Some(slot) = stamp.get_mut(r as usize) else {
                        break;
                    };
                    if *slot == me {
                        continue;
                    }
                    if count >= MAX_MICRODATA_PROPERTIES {
                        self.truncated = true;
                        return;
                    }
                    *slot = me;
                    list.push(r);
                    count += 1;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use holtmail_insight::limits::MAX_DISPLAY_CHARS;
    use holtmail_insight::{HtmlView, MicrodataValue};

    use crate::scan;

    fn props(view: &HtmlView, item: usize) -> Vec<(String, MicrodataValue)> {
        view.microdata
            .get(item)
            .map(|item| {
                item.properties
                    .iter()
                    .map(|p| (p.name.clone(), p.value.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn text(value: &str) -> MicrodataValue {
        MicrodataValue::Text(value.into())
    }

    #[test]
    fn body_itemscope_is_the_base_item() {
        let view = scan(
            "<html><body itemscope itemtype=\"https://schema.org/EmailMessage\">\
             <p itemprop=\"description\">Hello</p><div itemprop=\"x y\">two names</div></body></html>",
        );
        assert_eq!(view.microdata.len(), 1);
        assert!(view.microdata.first().is_some_and(|i| i.top_level));
        assert_eq!(
            props(&view, 0),
            [
                ("description".into(), text("Hello")),
                ("x".into(), text("two names")),
                ("y".into(), text("two names")),
            ]
        );
    }

    #[test]
    fn values_follow_the_element() {
        let long = "y".repeat(MAX_DISPLAY_CHARS + 10);
        let html = format!(
            "<div itemscope><iframe itemprop=\"frame\" src=\"https://example.com/f\"></iframe>\
             <title itemprop=\"ignored\">t</title><meter itemprop=\"m\" value=\"3\">three</meter>\
             <time itemprop=\"t\">tomorrow</time><meta itemprop=\"empty\">\
             <a itemprop=\"huge\" href=\"https://example.com/{}\">x</a>\
             <span itemprop=\"long\">{long}</span><source itemprop=\"src\" src=\"https://example.org/s\">\
             <meta itemprop=\"leaf\" itemscope itemtype=\"https://schema.org/Thing\"></div>",
            "a".repeat(9_000)
        );
        let view = scan(&html);
        let found = props(&view, 0);
        let mut cut: String = long.chars().take(MAX_DISPLAY_CHARS - 1).collect();
        cut.push('…');
        assert_eq!(
            found,
            [
                (
                    "frame".into(),
                    MicrodataValue::Url("https://example.com/f".into())
                ),
                ("m".into(), text("3")),
                ("t".into(), text("tomorrow")),
                ("empty".into(), text("")),
                ("long".into(), MicrodataValue::Text(cut)),
                (
                    "src".into(),
                    MicrodataValue::Url("https://example.org/s".into())
                ),
                ("leaf".into(), MicrodataValue::Item(1)),
            ]
        );
        assert!(view.microdata.get(1).is_some_and(|i| !i.top_level));
        assert!(view.truncated_structured);
    }

    #[test]
    fn misnested_markup_keeps_values_whole() {
        let view = scan(
            "<div itemscope><b itemprop=\"a\">one<p>two</b>three</p><i itemprop=\"b\">four</i></div>",
        );
        let found = props(&view, 0);
        assert_eq!(found.len(), 2);
        assert!(found.iter().any(|(n, v)| n == "b" && *v == text("four")));
        assert_eq!(
            scan("<b itemprop=x itemscope><p>a</b>b"),
            scan("<b itemprop=x itemscope><p>a</b>b")
        );
    }

    #[test]
    fn hidden_text_counts_but_inert_text_does_not() {
        let view = scan(
            "<div itemscope><span itemprop=\"n\">A<span hidden>B</span><template>C</template>D</span></div>",
        );
        assert_eq!(props(&view, 0), [("n".into(), text("AB D"))]);
    }
}
