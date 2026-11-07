use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::mem;

use holtmail_insight::limits::{
    MAX_ANCHORS, MAX_HTML_BYTES, MAX_LD_JSON_BLOCKS, MAX_LD_JSON_BYTES, MAX_SEGMENTS,
};
use holtmail_insight::{Anchor, HtmlView, Segment};
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::states::RawKind;
use html5ever::tokenizer::{
    BufferQueue, Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer, TokenizerOpts,
};
use html5ever::{LocalName, TokenizerResult, local_name};

use crate::forward::is_marker;
use crate::microdata::{Attrs, Leaf, Micro, Microdata, Pos, below};
use crate::names::Kind;
use crate::style::{Style, large_font_attribute};

const MAX_DEPTH: usize = 256;

const UNSUBSCRIBE: &[&str] = &[
    "unsubscribe",
    "opt out",
    "opt-out",
    "abmelden",
    "abbestellen",
    "désabonner",
    "desabonner",
    "désinscri",
    "desinscri",
    "отписаться",
    "отписка",
    "darse de baja",
    "cancelar suscripción",
    "uitschrijven",
    "afmelden",
    "disiscriviti",
    "cancelar inscrição",
    "descadastr",
];

/// Scans message HTML into its visible anchors and text segments, its `ld+json` blocks, its
/// microdata items and its footer marker.
///
/// - Reads at most [`MAX_HTML_BYTES`] of `html` (cut on a char boundary); the rest is ignored.
/// - Tokenizes as a browser does: `title` and `textarea` are RCDATA; `style`, `xmp`, `iframe`,
///   `noembed`, `noframes` and `noscript` are RAWTEXT; `script` is script data; `plaintext` takes
///   the rest of the input. None of their text is ever a segment or anchor text. SVG and MathML
///   content is tokenized as foreign content.
/// - Hidden subtrees are skipped: the `hidden` attribute, `aria-hidden="true"`, inline
///   `display:none`, `visibility:hidden`, `font-size:0`, `opacity:0`, `max-height:0` with
///   `overflow:hidden`, `template`, `noscript`, `object`, `applet`, `select`, `datalist`, SVG and
///   MathML, and HTML elements named like SVG or MathML elements (the sanitizer drops those).
/// - Anchors are reported only for `<a>` elements whose `href` parses as an absolute `http`,
///   `https` or `mailto` URL; at most [`MAX_ANCHORS`]. Segments: at most [`MAX_SEGMENTS`].
/// - `ld_json` holds the text of `<script type="application/ld+json">` blocks outside templates
///   and foreign content, CSS-hidden or not; at most [`MAX_LD_JSON_BLOCKS`], and a block over
///   [`MAX_LD_JSON_BYTES`] is dropped, never cut. `ld_json_forward_at` is the first block below
///   the forward marker.
/// - `microdata` holds the WHATWG microdata items, flat and in document order, collected like
///   `ld_json` (CSS- and `aria`-hidden subtrees included, never inside templates, foreign content,
///   raw-text elements or names the sanitizer drops). Item nesting is at most
///   [`MAX_MICRODATA_DEPTH`]; items and properties, `itemref` expansion included, are each at most
///   [`MAX_MICRODATA_PROPERTIES`]. Every [`Item`](holtmail_insight::MicrodataValue::Item) index is
///   inside the list. `itemref` cycles and self-references are cut.
/// - `truncated_structured` is set whenever a JSON-LD or microdata cap left something out.
/// - Every anchor `href` and every word of every segment also survives `holtmail-sanitize` on the
///   same input; the crate docs list the rare adversarial markup where this is not yet so.
/// - Skipped content separates words: text on either side of it never joins into one word.
/// - Element nesting deeper than 256 is flattened; nothing recurses over the input, so memory is
///   linear in the input and stack use is constant.
/// - Deterministic and never panics.
///
/// [`MAX_HTML_BYTES`]: holtmail_insight::limits::MAX_HTML_BYTES
/// [`MAX_ANCHORS`]: holtmail_insight::limits::MAX_ANCHORS
/// [`MAX_SEGMENTS`]: holtmail_insight::limits::MAX_SEGMENTS
/// [`MAX_LD_JSON_BLOCKS`]: holtmail_insight::limits::MAX_LD_JSON_BLOCKS
/// [`MAX_LD_JSON_BYTES`]: holtmail_insight::limits::MAX_LD_JSON_BYTES
/// [`MAX_MICRODATA_DEPTH`]: holtmail_insight::limits::MAX_MICRODATA_DEPTH
/// [`MAX_MICRODATA_PROPERTIES`]: holtmail_insight::limits::MAX_MICRODATA_PROPERTIES
pub fn scan(html: &str) -> HtmlView {
    let html = html
        .get(..html.floor_char_boundary(MAX_HTML_BYTES))
        .unwrap_or_default();
    let tokenizer = Tokenizer::new(Sink(RefCell::default()), TokenizerOpts::default());
    let queue = BufferQueue::default();
    queue.push_back(StrTendril::from_slice(html));
    while !matches!(tokenizer.feed(&queue), TokenizerResult::Done) {}
    tokenizer.end();
    tokenizer.sink.0.into_inner().finish()
}

struct Sink(RefCell<Scanner>);

impl TokenSink for Sink {
    type Handle = ();

    fn process_token(&self, token: Token, _line: u64) -> TokenSinkResult<()> {
        let mut scanner = self.0.borrow_mut();
        match token {
            Token::TagToken(tag) => match tag.kind {
                TagKind::StartTag => scanner.start(&tag),
                TagKind::EndTag => {
                    scanner.end(&tag.name);
                    TokenSinkResult::Continue
                }
            },
            Token::CharacterTokens(text) => {
                scanner.text(&text);
                TokenSinkResult::Continue
            }
            _ => TokenSinkResult::Continue,
        }
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.0
            .borrow()
            .stack
            .last()
            .is_some_and(|frame| frame.ns != Ns::Html)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ns {
    Html,
    Svg,
    Math,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Default,
    ListItem,
    Button,
    Table,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Scope(Scope),
    Special,
    ListItemSearch,
    Marker,
    Html,
}

impl Stop {
    const ALL: [Stop; 8] = [
        Stop::Scope(Scope::Default),
        Stop::Scope(Scope::ListItem),
        Stop::Scope(Scope::Button),
        Stop::Scope(Scope::Table),
        Stop::Special,
        Stop::ListItemSearch,
        Stop::Marker,
        Stop::Html,
    ];

    fn slot(self) -> usize {
        match self {
            Stop::Scope(Scope::Default) => 0,
            Stop::Scope(Scope::ListItem) => 1,
            Stop::Scope(Scope::Button) => 2,
            Stop::Scope(Scope::Table) => 3,
            Stop::Special => 4,
            Stop::ListItemSearch => 5,
            Stop::Marker => 6,
            Stop::Html => 7,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Derived {
    stops: [u32; 8],
    table_mode: bool,
    in_template: bool,
    in_table: bool,
}

#[derive(Default)]
struct AtomHasher(u64);

impl Hasher for AtomHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 =
                (self.0.rotate_left(5) ^ u64::from(*byte)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(n)).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }
}

type Positions = HashMap<LocalName, Vec<u32>, BuildHasherDefault<AtomHasher>>;

#[derive(Debug, Clone, Copy, Default)]
struct Ctx {
    hidden: bool,
    inert: bool,
    emphasis: bool,
    cell_button: bool,
}

#[derive(Debug)]
struct Frame {
    name: LocalName,
    ns: Ns,
    own: Own,
    ctx: Ctx,
    anchor: bool,
    block: bool,
    kind: Kind,
    derived: Derived,
    micro: Micro,
}

#[derive(Debug, Clone, Copy, Default)]
struct Own {
    hidden: bool,
    inert: bool,
    emphasis: bool,
    cell_button: Option<bool>,
}

impl Own {
    fn under(self, parent: Ctx) -> Ctx {
        Ctx {
            hidden: parent.hidden || self.hidden,
            inert: parent.inert || self.inert,
            emphasis: parent.emphasis || self.emphasis,
            cell_button: self.cell_button.unwrap_or(parent.cell_button),
        }
    }
}

impl Frame {
    fn is(&self, name: &LocalName) -> bool {
        self.ns == Ns::Html && self.name == *name
    }

    fn has(&self, bit: u32) -> bool {
        self.ns == Ns::Html && self.kind.has(bit)
    }

    fn integration(&self) -> bool {
        match self.ns {
            Ns::Html => false,
            Ns::Svg => self.kind.has(Kind::SVG_INTEGRATION),
            Ns::Math => self.kind.has(Kind::MATH_INTEGRATION),
        }
    }

    fn special(&self) -> bool {
        match self.ns {
            Ns::Html => self.kind.has(Kind::SPECIAL),
            _ => self.integration(),
        }
    }

    fn stops(&self, stop: Stop) -> bool {
        match stop {
            Stop::Scope(scope) => self.bounds(scope),
            Stop::Special => self.special(),
            Stop::ListItemSearch => self.special() && !self.has(Kind::LIST_ITEM_PASS),
            Stop::Marker => self.has(Kind::MARKER),
            Stop::Html => self.ns == Ns::Html,
        }
    }

    fn derive(&mut self, parent: Option<&Derived>, index: usize) {
        let mut derived = parent.copied().unwrap_or_default();
        let at = u32::try_from(index + 1).unwrap_or(u32::MAX);
        for stop in Stop::ALL {
            if self.stops(stop)
                && let Some(slot) = derived.stops.get_mut(stop.slot())
            {
                *slot = at;
            }
        }
        if self.has(Kind::TABLE_MODE_OFF) {
            derived.table_mode = false;
        } else if self.has(Kind::TABLE_MODE_ON) {
            derived.table_mode = true;
        }
        let template = self.is(&local_name!("template"));
        derived.in_template |= template;
        derived.in_table |= template || self.is(&local_name!("table"));
        self.derived = derived;
    }

    fn bounds(&self, scope: Scope) -> bool {
        if self.ns != Ns::Html {
            return scope != Scope::Table && self.integration();
        }
        match scope {
            Scope::Table => self.kind.has(Kind::BOUND_TABLE),
            _ => {
                self.kind.has(Kind::BOUND)
                    || (scope == Scope::ListItem && self.kind.has(Kind::BOUND_LIST))
                    || (scope == Scope::Button && self.kind.has(Kind::BOUND_BUTTON))
            }
        }
    }
}

#[derive(Debug, Default)]
enum Raw {
    #[default]
    None,
    Skip {
        separates: bool,
    },
    LdJson(String, bool),
}

type Entry = (LocalName, Own, Kind);

#[derive(Debug)]
struct Reopen {
    groups: Vec<Vec<Entry>>,
    len: usize,
    spare: Vec<Entry>,
}

impl Default for Reopen {
    fn default() -> Self {
        Reopen {
            groups: vec![Vec::new()],
            len: 0,
            spare: Vec::new(),
        }
    }
}

impl Reopen {
    fn mark(&mut self) {
        self.groups.push(Vec::new());
    }

    fn unmark(&mut self) {
        if self.groups.len() > 1 {
            if let Some(group) = self.groups.pop() {
                self.len -= group.len();
            }
        } else if let Some(group) = self.groups.first_mut() {
            self.len -= group.len();
            group.clear();
        }
    }

    fn take(&mut self) -> Vec<Entry> {
        let spare = mem::take(&mut self.spare);
        let group = match self.groups.last_mut() {
            Some(group) => mem::replace(group, spare),
            None => spare,
        };
        self.len -= group.len();
        group
    }

    fn recycle(&mut self, mut entries: Vec<Entry>) {
        entries.clear();
        if entries.capacity() > self.spare.capacity() {
            self.spare = entries;
        }
    }

    fn extend(&mut self, entries: &mut Vec<Entry>) {
        self.len += entries.len();
        if let Some(group) = self.groups.last_mut() {
            group.extend(entries.drain(..).rev());
        }
        if self.len > MAX_REOPEN
            && let Some(group) = self.groups.iter_mut().find(|g| !g.is_empty())
        {
            group.remove(0);
            self.len -= 1;
        }
    }

    fn remove_current(&mut self, name: &LocalName) -> bool {
        let Some(group) = self.groups.last_mut() else {
            return false;
        };
        let Some(entry) = group.iter().rposition(|(n, _, _)| n == name) else {
            return false;
        };
        group.remove(entry);
        self.len -= 1;
        true
    }
}

#[derive(Debug)]
struct Open {
    anchor: Anchor,
    space: bool,
    segment: usize,
}

#[derive(Default)]
struct Scanner {
    stack: Vec<Frame>,
    deep_hidden: bool,
    deep_inert: bool,
    raw: Raw,
    anchors: Vec<Anchor>,
    open: Option<Open>,
    segments: Vec<Segment>,
    buf: String,
    buf_space: bool,
    buf_emphasis: bool,
    buf_anchor: u32,
    line_start: usize,
    forward: Option<(usize, u32)>,
    footer: Option<(usize, u32)>,
    ld_json: Vec<String>,
    ld_json_at: Vec<Pos>,
    ld_json_truncated: bool,
    micro: Microdata,
    base: Option<u32>,
    base_body: bool,
    reopen: Reopen,
    form_open: bool,
    html_at: Positions,
    foreign_at: Positions,
    kinds: HashMap<LocalName, Kind, BuildHasherDefault<AtomHasher>>,
    closed: Vec<Entry>,
}

const MAX_REOPEN: usize = 32;

const MAX_KINDS: usize = 1024;

const MAX_IDLE_NAMES: usize = 64;

impl Scanner {
    fn ctx(&self) -> Ctx {
        let mut ctx = self.stack.last().map(|frame| frame.ctx).unwrap_or_default();
        ctx.hidden |= self.deep_hidden || self.deep_inert;
        ctx.inert |= self.deep_inert;
        ctx
    }

    fn foreign_mode(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|frame| frame.ns != Ns::Html && !frame.integration())
    }

    fn anchor_count(&self) -> u32 {
        u32::try_from(self.anchors.len() + usize::from(self.open.is_some())).unwrap_or(u32::MAX)
    }

    fn pos(&self) -> Pos {
        (self.segments.len(), !self.buf.is_empty())
    }

    fn scope(&self) -> Option<u32> {
        self.stack
            .last()
            .map_or(self.base, |frame| frame.micro.scope)
    }

    fn micro_leaf(&mut self, tag: &Tag, leaf: Leaf) {
        if let Some(attrs) = Attrs::of(tag) {
            let (scope, at) = (self.scope(), self.pos());
            self.micro.open(tag, &attrs, scope, at, leaf);
        }
    }

    fn micro_root(&mut self, tag: &Tag) {
        let body = tag.name == local_name!("body");
        if self.ctx().inert || (body && self.base_body) || (!body && self.base.is_some()) {
            return;
        }
        let Some(attrs) = Attrs::of(tag).filter(Attrs::itemscope) else {
            return;
        };
        let (scope, at) = (self.base, self.pos());
        let micro = self.micro.open(tag, &attrs, scope, at, Leaf::Root);
        if let Some(item) = micro.scoped(None).scope {
            self.base = Some(item);
            self.base_body |= body;
        }
    }

    fn kind(&mut self, name: &LocalName) -> Kind {
        if let Some(kind) = self.kinds.get(name) {
            return *kind;
        }
        let kind = Kind::of(name);
        if self.kinds.len() < MAX_KINDS {
            self.kinds.insert(name.clone(), kind);
        }
        kind
    }

    fn start(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let kind = self.kind(&tag.name);
        if self.foreign_mode() {
            let font_breaks = tag.name == local_name!("font")
                && tag
                    .attrs
                    .iter()
                    .any(|a| matches!(&*a.name.local, "color" | "face" | "size"));
            if kind.has(Kind::BREAKOUT) || font_breaks {
                while self
                    .stack
                    .last()
                    .is_some_and(|frame| frame.ns != Ns::Html && !frame.integration())
                {
                    self.pop();
                }
            } else {
                let ns = self.stack.last().map_or(Ns::Html, |frame| frame.ns);
                if !tag.self_closing {
                    self.push_foreign(tag.name.clone(), ns, kind);
                }
                return TokenSinkResult::Continue;
            }
        }
        self.html_start(tag, kind)
    }

    fn html_start(&mut self, tag: &Tag, kind: Kind) -> TokenSinkResult<()> {
        let name = &tag.name;
        if kind.has(Kind::STRUCTURE) {
            if *name == local_name!("html") || *name == local_name!("body") {
                self.micro_root(tag);
            }
            return TokenSinkResult::Continue;
        }
        if kind.has(Kind::FOREIGN_ROOT) {
            self.reconstruct();
            if !tag.self_closing {
                let ns = if *name == local_name!("svg") {
                    Ns::Svg
                } else {
                    Ns::Math
                };
                self.push_foreign(name.clone(), ns, kind);
            }
            return TokenSinkResult::Continue;
        }
        if kind.has(Kind::TABLE_PART) {
            if !self.top().in_table {
                return TokenSinkResult::Continue;
            }
            self.close_in_scope(&CELLS, Scope::Table);
            self.close_in_scope(&[local_name!("caption")], Scope::Table);
            let context: &[&str] = match &**name {
                "td" | "th" => &["table", "tbody", "thead", "tfoot", "tr", "template"],
                "tr" => &["table", "tbody", "thead", "tfoot", "template"],
                _ => &["table", "template"],
            };
            while self
                .stack
                .last()
                .is_some_and(|f| f.ns != Ns::Html || !context.contains(&&*f.name))
            {
                self.pop();
            }
        }
        let form = *name == local_name!("form");
        if self.top().table_mode {
            if form || *name == local_name!("input") {
                if form && !self.top().in_template {
                    self.form_open = true;
                }
                return TokenSinkResult::Continue;
            }
            if *name == local_name!("table") {
                self.close_in_scope(&[local_name!("table")], Scope::Table);
            }
        }
        if form {
            if self.form_open && !self.top().in_template {
                return TokenSinkResult::Continue;
            }
            self.form_open = true;
        }
        if *name == local_name!("select")
            && self.try_close(&[local_name!("select")], Scope::Default)
        {
            return TokenSinkResult::Continue;
        }
        self.implicit_close(name, kind);
        if !kind.has(Kind::NO_RECONSTRUCT) {
            self.reconstruct();
        }
        if kind.has(Kind::RAW) {
            return self.raw_start(tag);
        }
        if kind.has(Kind::VOID) {
            if *name == local_name!("br") || *name == local_name!("hr") {
                self.line_break();
            }
            if !self.ctx().inert && !kind.has(Kind::DROPPED) {
                self.micro_leaf(tag, Leaf::Void);
            }
            return TokenSinkResult::Continue;
        }
        let mut frame = self.frame_for(tag, kind);
        if frame.block {
            self.line_break();
        }
        if frame.anchor && self.stack.len() < MAX_DEPTH {
            self.open_anchor(tag, frame.ctx.hidden);
        }
        if !frame.ctx.inert
            && let Some(attrs) = Attrs::of(tag)
        {
            let leaf = if self.stack.len() < MAX_DEPTH {
                Leaf::Element
            } else {
                Leaf::Flat
            };
            let (scope, at) = (self.scope(), self.pos());
            let micro = self.micro.open(tag, &attrs, scope, at, leaf);
            if leaf == Leaf::Element {
                frame.micro = micro;
            }
        }
        self.push(frame);
        TokenSinkResult::Continue
    }

    fn raw_start(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name = &*tag.name;
        let kind = match name {
            "script" => RawKind::ScriptData,
            "title" | "textarea" => RawKind::Rcdata,
            "plaintext" => {
                self.raw = Raw::Skip { separates: true };
                return TokenSinkResult::Plaintext;
            }
            _ => RawKind::Rawtext,
        };
        if !self.ctx().inert {
            self.micro_leaf(tag, Leaf::Raw);
        }
        let ld_json = name == "script"
            && !self.ctx().inert
            && attribute(tag, "type").is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .eq_ignore_ascii_case("application/ld+json")
            });
        self.raw = if ld_json {
            Raw::LdJson(String::new(), false)
        } else {
            Raw::Skip {
                separates: !matches!(name, "script" | "style"),
            }
        };
        TokenSinkResult::RawData(kind)
    }

    fn frame_for(&self, tag: &Tag, kind: Kind) -> Frame {
        let parent = self.ctx();
        let name = &tag.name;
        let attrs = !tag.attrs.is_empty();
        let style = attribute(tag, "style")
            .map(Style::parse)
            .unwrap_or_default();
        let dropped = kind.has(Kind::DROPPED);
        let hidden = (attrs
            && (attribute(tag, "hidden").is_some()
                || attribute(tag, "aria-hidden")
                    .is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
                || style.hides()))
            || kind.has(Kind::SKIPS_SUBTREE)
            || dropped;
        let emphasis = kind.has(Kind::EMPHASIS)
            || (attrs
                && (style.emphasis()
                    || (*name == local_name!("font")
                        && attribute(tag, "size").is_some_and(large_font_attribute))));
        let cell_button = kind.has(Kind::CELL).then(|| {
            style.button()
                || attribute(tag, "bgcolor").is_some_and(|v| !v.trim().is_empty())
                || attribute(tag, "background").is_some_and(|v| !v.trim().is_empty())
        });
        let own = Own {
            hidden,
            inert: *name == local_name!("template") || dropped,
            emphasis,
            cell_button,
        };
        Frame {
            name: name.clone(),
            ns: Ns::Html,
            own,
            ctx: own.under(parent),
            anchor: *name == local_name!("a"),
            block: kind.has(Kind::BLOCK),
            kind,
            derived: Derived::default(),
            micro: Micro::default(),
        }
    }

    fn push_foreign(&mut self, name: LocalName, ns: Ns, kind: Kind) {
        let own = Own {
            hidden: true,
            inert: true,
            ..Own::default()
        };
        let ctx = own.under(self.ctx());
        self.push(Frame {
            name,
            ns,
            own,
            ctx,
            anchor: false,
            block: false,
            kind,
            derived: Derived::default(),
            micro: Micro::default(),
        });
    }

    fn top(&self) -> Derived {
        self.stack
            .last()
            .map(|frame| frame.derived)
            .unwrap_or_default()
    }

    fn positions(&mut self, ns: Ns) -> &mut Positions {
        if ns == Ns::Html {
            &mut self.html_at
        } else {
            &mut self.foreign_at
        }
    }

    fn top_of(&self, names: &[LocalName]) -> Option<usize> {
        names
            .iter()
            .filter_map(|name| self.html_at.get(name).and_then(|at| at.last()))
            .max()
            .map(|at| *at as usize)
    }

    fn stop(&self, stop: Stop) -> usize {
        self.stack
            .last()
            .and_then(|frame| frame.derived.stops.get(stop.slot()))
            .map_or(0, |at| *at as usize)
    }

    fn find(&self, names: &[LocalName], stop: Stop) -> Option<usize> {
        self.top_of(names).filter(|at| at + 1 >= self.stop(stop))
    }

    fn push(&mut self, mut frame: Frame) {
        if frame.has(Kind::MARKER) && self.stack.len() < MAX_DEPTH {
            self.reopen.mark();
        }
        if self.stack.len() >= MAX_DEPTH {
            let parent = self.ctx();
            self.deep_inert |= frame.ctx.inert && !parent.inert;
            self.deep_hidden |= frame.ctx.hidden;
            return;
        }
        let index = self.stack.len();
        frame.micro = frame.micro.scoped(self.scope());
        frame.derive(self.stack.last().map(|f| &f.derived), index);
        let at = u32::try_from(index).unwrap_or(u32::MAX);
        self.positions(frame.ns)
            .entry(frame.name.clone())
            .or_default()
            .push(at);
        self.stack.push(frame);
    }

    fn forget(&mut self, frame: &Frame, index: usize) {
        let positions = self.positions(frame.ns);
        if let Some(at) = positions.get_mut(&frame.name) {
            if let Some(slot) = at.iter().rposition(|i| *i as usize == index) {
                at.remove(slot);
            }
            if at.is_empty() && positions.len() > MAX_IDLE_NAMES {
                positions.remove(&frame.name);
            }
        }
    }

    fn pop(&mut self) -> Option<Frame> {
        let frame = self.stack.pop()?;
        self.forget(&frame, self.stack.len());
        self.micro.close(frame.micro);
        self.deep_hidden = false;
        if frame.anchor {
            self.close_anchor();
        }
        if frame.block {
            self.line_break();
        }
        if frame.has(Kind::MARKER) {
            self.reopen.unmark();
        }
        Some(frame)
    }

    fn reconstruct(&mut self) {
        let mut group = self.reopen.take();
        if group.is_empty() {
            self.reopen.recycle(group);
            return;
        }
        for (name, own, kind) in group.drain(..) {
            let ctx = own.under(self.ctx());
            self.push(Frame {
                name,
                ns: Ns::Html,
                own,
                ctx,
                anchor: false,
                block: false,
                kind,
                derived: Derived::default(),
                micro: Micro::default(),
            });
        }
        self.reopen.recycle(group);
    }

    fn remove(&mut self, index: usize) {
        if index >= self.stack.len() {
            return;
        }
        let frame = self.stack.remove(index);
        self.forget(&frame, index);
        self.micro.close(frame.micro);
        for at in self
            .html_at
            .values_mut()
            .chain(self.foreign_at.values_mut())
        {
            for i in at.iter_mut().filter(|i| **i as usize > index) {
                *i -= 1;
            }
        }
        if frame.anchor {
            self.close_anchor();
        }
        if frame.block {
            self.line_break();
        }
        let below = index.checked_sub(1).and_then(|i| self.stack.get(i));
        let mut parent = below.map(|f| f.ctx).unwrap_or_default();
        let mut derived = below.map(|f| f.derived);
        let mut scope = below.map_or(self.base, |f| f.micro.scope);
        for (at, frame) in self.stack.iter_mut().enumerate().skip(index) {
            frame.ctx = frame.own.under(parent);
            parent = frame.ctx;
            frame.micro = frame.micro.scoped(scope);
            scope = frame.micro.scope;
            frame.derive(derived.as_ref(), at);
            derived = Some(frame.derived);
        }
    }

    fn implied_end(&mut self) {
        while self.stack.last().is_some_and(|f| f.has(Kind::IMPLIED_END)) {
            self.pop();
        }
    }

    fn adopt(&mut self, formatting: usize) {
        let mut formatting = formatting;
        let mut remove_formatting = true;
        for _ in 0..8 {
            let furthest = self
                .stack
                .iter()
                .enumerate()
                .skip(formatting + 1)
                .find(|(_, frame)| frame.special())
                .map(|(index, _)| index);
            let Some(furthest) = furthest else {
                if remove_formatting {
                    self.pop_to(formatting);
                } else {
                    self.pop_to(formatting + 1);
                }
                return;
            };
            let mut furthest = furthest;
            for index in (formatting + 1..furthest).rev() {
                if self
                    .stack
                    .get(index)
                    .is_some_and(|f| !f.has(Kind::FORMATTING))
                {
                    self.remove(index);
                    furthest -= 1;
                }
            }
            if remove_formatting {
                self.remove(formatting);
                furthest -= 1;
            }
            formatting = furthest;
            remove_formatting = false;
        }
    }

    fn pop_to(&mut self, index: usize) {
        let mut closed = mem::take(&mut self.closed);
        closed.clear();
        while self.stack.len() > index {
            let Some(frame) = self.pop() else {
                break;
            };
            if frame.has(Kind::MARKER) {
                closed.clear();
            }
            if self.stack.len() > index && frame.has(Kind::FORMATTING) {
                closed.push((frame.name, frame.own, frame.kind));
            }
        }
        self.reopen.extend(&mut closed);
        self.closed = closed;
    }

    fn close_in_scope(&mut self, names: &[LocalName], scope: Scope) {
        self.try_close(names, scope);
    }

    fn try_close(&mut self, names: &[LocalName], scope: Scope) -> bool {
        let Some(index) = self.find(names, Stop::Scope(scope)) else {
            return false;
        };
        self.pop_to(index);
        true
    }

    fn close_list_item(&mut self, names: &[LocalName]) {
        if let Some(index) = self.find(names, Stop::ListItemSearch) {
            self.pop_to(index);
        }
    }

    fn implicit_close(&mut self, name: &LocalName, kind: Kind) {
        if kind.has(Kind::CLOSES_P) && self.html_at.contains_key(&local_name!("p")) {
            self.close_in_scope(&[local_name!("p")], Scope::Button);
        }
        if !kind.has(Kind::IMPLICIT_CLOSE) {
            return;
        }
        match &**name {
            _ if kind.has(Kind::HEADING) => {
                if self.stack.last().is_some_and(|f| f.has(Kind::HEADING)) {
                    self.pop();
                }
            }
            "li" => self.close_list_item(&[local_name!("li")]),
            "dd" | "dt" => self.close_list_item(&[local_name!("dd"), local_name!("dt")]),
            "a" => {
                if let Some(index) = self.find(&[local_name!("a")], Stop::Marker) {
                    if self.stop(Stop::Scope(Scope::Default)) <= index {
                        self.adopt(index);
                    }
                    if self
                        .stack
                        .get(index)
                        .is_some_and(|f| f.is(&local_name!("a")))
                    {
                        self.remove(index);
                    }
                }
            }
            "td" | "th" => self.close_in_scope(&CELLS, Scope::Table),
            "tr" => {
                self.close_in_scope(&CELLS, Scope::Table);
                self.close_in_scope(&[local_name!("tr")], Scope::Table);
            }
            "tbody" | "thead" | "tfoot" => {
                self.close_in_scope(&CELLS, Scope::Table);
                self.close_in_scope(&[local_name!("tr")], Scope::Table);
                self.close_in_scope(
                    &[
                        local_name!("tbody"),
                        local_name!("thead"),
                        local_name!("tfoot"),
                    ],
                    Scope::Table,
                );
            }
            "option" => {
                if self
                    .stack
                    .last()
                    .is_some_and(|f| f.is(&local_name!("option")))
                {
                    self.pop();
                }
            }
            "optgroup" => {
                if self
                    .stack
                    .last()
                    .is_some_and(|f| f.is(&local_name!("option")))
                {
                    self.pop();
                }
                if self
                    .stack
                    .last()
                    .is_some_and(|f| f.is(&local_name!("optgroup")))
                {
                    self.pop();
                }
            }
            "button" => self.close_in_scope(&[local_name!("button")], Scope::Default),
            "input" => self.close_in_scope(&[local_name!("select")], Scope::Default),
            _ => {}
        }
    }

    fn end(&mut self, name: &LocalName) {
        if !matches!(self.raw, Raw::None) {
            self.end_raw();
            return;
        }
        if self.stack.last().is_some_and(|frame| frame.ns != Ns::Html) {
            if matches!(&**name, "br" | "p") {
                while self
                    .stack
                    .last()
                    .is_some_and(|frame| frame.ns != Ns::Html && !frame.integration())
                {
                    self.pop();
                }
            } else if let Some(index) = self
                .foreign_at
                .get(name)
                .and_then(|at| at.last())
                .map(|at| *at as usize)
                .filter(|at| at + 1 > self.stop(Stop::Html))
            {
                self.pop_to(index);
                return;
            }
        }
        self.html_end(name);
    }

    fn html_end(&mut self, tag: &LocalName) {
        let kind = self.kind(tag);
        if !kind.has(Kind::END_RULE) {
            if kind.has(Kind::FORMATTING) {
                if self.reopen.remove_current(tag) {
                    return;
                }
                if let Some(index) =
                    self.find(std::slice::from_ref(tag), Stop::Scope(Scope::Default))
                {
                    self.adopt(index);
                }
            } else if kind.has(Kind::SPECIAL) {
                self.close_in_scope(std::slice::from_ref(tag), Scope::Default);
            } else if let Some(index) = self.find(std::slice::from_ref(tag), Stop::Special) {
                self.pop_to(index);
            }
            return;
        }
        match &**tag {
            "br" => self.line_break(),
            "html" | "head" | "body" | "frameset" => {}
            "p" => {
                if !self.try_close(&[local_name!("p")], Scope::Button) {
                    self.line_break();
                }
            }
            "li" => self.close_in_scope(&[local_name!("li")], Scope::ListItem),
            _ if kind.has(Kind::HEADING) => self.close_in_scope(&HEADINGS, Scope::Default),
            "tr" | "tbody" | "thead" | "tfoot" => {
                let _ = self.try_close(std::slice::from_ref(tag), Scope::Table)
                    || self.try_close(&[local_name!("tr")], Scope::Table)
                    || self.try_close(&CELLS, Scope::Table);
            }
            "td" | "th" | "table" | "caption" | "colgroup" => {
                self.close_in_scope(std::slice::from_ref(tag), Scope::Table);
            }
            "form" => {
                self.form_open = false;
                if let Some(index) = self.find(&[local_name!("form")], Stop::Scope(Scope::Default))
                {
                    if self.top().in_template {
                        self.pop_to(index);
                    } else {
                        self.implied_end();
                        self.remove(index);
                    }
                }
            }
            "template" => {
                if let Some(index) = self.top_of(&[local_name!("template")]) {
                    self.pop_to(index);
                }
            }
            _ => {}
        }
    }

    fn end_raw(&mut self) {
        let Raw::LdJson(block, over) = mem::take(&mut self.raw) else {
            return;
        };
        if over {
            self.ld_json_truncated = true;
        } else if !block.trim().is_empty() {
            if self.ld_json.len() < MAX_LD_JSON_BLOCKS {
                self.ld_json.push(block);
                self.ld_json_at.push(self.pos());
            } else {
                self.ld_json_truncated = true;
            }
        }
    }

    fn open_anchor(&mut self, tag: &Tag, hidden: bool) {
        self.close_anchor();
        if hidden || self.anchors.len() >= MAX_ANCHORS {
            return;
        }
        let Some(href) = attribute(tag, "href").filter(|href| allowed_href(href)) else {
            return;
        };
        let own = attribute(tag, "style")
            .map(Style::parse)
            .unwrap_or_default();
        let button_like = self.ctx().cell_button
            || own.button()
            || attribute(tag, "role").is_some_and(|r| r.trim().eq_ignore_ascii_case("button"));
        self.open = Some(Open {
            anchor: Anchor {
                href: href.to_owned(),
                text: String::new(),
                aria_label: attribute(tag, "aria-label").map(str::to_owned),
                button_like,
                in_footer: false,
                below_forward: false,
                index: u32::try_from(self.anchors.len()).unwrap_or(u32::MAX),
            },
            space: false,
            segment: self.segments.len(),
        });
    }

    fn close_anchor(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        if self.footer.is_none() && is_unsubscribe(&open.anchor) {
            self.footer = Some((open.segment, open.anchor.index));
        }
        self.anchors.push(open.anchor);
    }

    fn text(&mut self, text: &str) {
        match &mut self.raw {
            Raw::None => {}
            Raw::Skip { separates } => {
                if *separates {
                    self.separate();
                }
                return;
            }
            Raw::LdJson(block, over) => {
                if !*over {
                    if block.len() + text.len() > MAX_LD_JSON_BYTES {
                        *over = true;
                        *block = String::new();
                    } else {
                        block.push_str(text);
                    }
                }
                return;
            }
        }
        self.reconstruct();
        let ctx = self.ctx();
        if !ctx.inert {
            self.micro.text(text);
        }
        if ctx.hidden {
            self.separate();
            return;
        }
        if let Some(open) = &mut self.open {
            collapse(&mut open.anchor.text, &mut open.space, text);
        }
        if self.segments.len() >= MAX_SEGMENTS {
            return;
        }
        if !self.buf.is_empty()
            && self.buf_emphasis != ctx.emphasis
            && text.chars().any(|c| !c.is_whitespace() && !invisible(c))
        {
            self.flush();
        }
        let was_empty = self.buf.is_empty();
        collapse(&mut self.buf, &mut self.buf_space, text);
        if was_empty && !self.buf.is_empty() {
            self.buf_emphasis = ctx.emphasis;
            self.buf_anchor = self.anchor_count();
        }
        if self.footer.is_none() && text.contains('©') {
            self.footer = Some((self.segments.len(), self.anchor_count()));
        }
    }

    fn separate(&mut self) {
        self.micro.separate();
        self.buf_space = true;
        if let Some(open) = &mut self.open {
            open.space = true;
        }
    }

    fn flush(&mut self) {
        self.buf_space = false;
        if self.buf.is_empty() {
            return;
        }
        let text = mem::take(&mut self.buf);
        if self.segments.len() >= MAX_SEGMENTS {
            return;
        }
        let index = self.segments.len();
        if self.forward.is_none() && index == self.line_start && is_marker(&text) {
            self.forward = Some((index, self.buf_anchor));
        }
        self.segments.push(Segment {
            text,
            emphasis: self.buf_emphasis,
            alone_on_line: false,
            below_forward: self.forward.is_some(),
        });
    }

    fn line_break(&mut self) {
        self.flush();
        self.micro.separate();
        if self.segments.len() == self.line_start + 1
            && let Some(segment) = self.segments.get_mut(self.line_start)
        {
            segment.alone_on_line = true;
        }
        self.line_start = self.segments.len();
        if let Some(open) = &mut self.open {
            open.space = true;
        }
    }

    fn finish(mut self) -> HtmlView {
        self.end_raw();
        self.pop_to(0);
        self.close_anchor();
        self.line_break();
        let forward_segment = self.forward.map(|(segment, _)| segment);
        let forward = self.forward.map(|(_, anchor)| anchor);
        let footer = self.footer.map(|(_, anchor)| anchor);
        for anchor in &mut self.anchors {
            anchor.below_forward = forward.is_some_and(|from| anchor.index >= from);
            anchor.in_footer = footer.is_some_and(|from| anchor.index >= from);
        }
        let ld_json_forward_at = self
            .ld_json_at
            .iter()
            .position(|at| below(*at, forward_segment));
        let (microdata, micro_truncated) = self.micro.finish(forward_segment);
        HtmlView::new()
            .with_anchors(self.anchors)
            .with_ld_json(self.ld_json)
            .with_ld_json_forward_at(ld_json_forward_at)
            .with_microdata(microdata)
            .with_segments(self.segments)
            .with_footer_at(self.footer.map(|(segment, _)| segment))
            .with_truncated_structured(self.ld_json_truncated || micro_truncated)
    }
}

const CELLS: [LocalName; 2] = [local_name!("td"), local_name!("th")];

const HEADINGS: [LocalName; 6] = [
    local_name!("h1"),
    local_name!("h2"),
    local_name!("h3"),
    local_name!("h4"),
    local_name!("h5"),
    local_name!("h6"),
];

fn attribute<'t>(tag: &'t Tag, name: &str) -> Option<&'t str> {
    tag.attrs
        .iter()
        .find(|a| &*a.name.local == name)
        .map(|a| &*a.value)
}

fn allowed_href(href: &str) -> bool {
    url::Url::parse(href).is_ok_and(|url| matches!(url.scheme(), "http" | "https" | "mailto"))
}

fn is_unsubscribe(anchor: &Anchor) -> bool {
    let text = anchor.text.to_lowercase();
    let label = anchor
        .aria_label
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    anchor.href.to_ascii_lowercase().contains("unsubscribe")
        || UNSUBSCRIBE
            .iter()
            .any(|word| text.contains(word) || label.contains(word))
}

fn invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' | '\u{034F}' | '\u{180E}' | '\u{200B}' | '\u{200C}' | '\u{2060}' | '\u{FEFF}'
    )
}

pub(crate) fn collapse(out: &mut String, space: &mut bool, text: &str) {
    for c in text.chars() {
        if invisible(c) {
            continue;
        }
        if c.is_whitespace() {
            *space = true;
            continue;
        }
        if *space && !out.is_empty() {
            out.push(' ');
        }
        *space = false;
        out.push(c);
    }
}
