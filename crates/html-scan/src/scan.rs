use std::cell::RefCell;
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
use html5ever::{LocalName, TokenizerResult};

use crate::forward::is_marker;
use crate::names;
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

/// Scans message HTML into its visible anchors and text segments, its `ld+json` blocks and its
/// footer marker.
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
///   [`MAX_LD_JSON_BYTES`] is dropped, never cut. `microdata` stays empty.
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
    fn is(&self, name: &str) -> bool {
        self.ns == Ns::Html && &*self.name == name
    }

    fn integration(&self) -> bool {
        match self.ns {
            Ns::Html => false,
            Ns::Svg => names::is_svg_integration(&self.name),
            Ns::Math => names::is_math_integration(&self.name),
        }
    }

    fn special(&self) -> bool {
        match self.ns {
            Ns::Html => names::is_special(&self.name),
            _ => self.integration(),
        }
    }

    fn bounds(&self, scope: Scope) -> bool {
        if self.ns != Ns::Html {
            return scope != Scope::Table && self.integration();
        }
        let name = &*self.name;
        match scope {
            Scope::Table => matches!(name, "html" | "table" | "template"),
            _ => {
                matches!(
                    name,
                    "applet"
                        | "caption"
                        | "html"
                        | "table"
                        | "td"
                        | "th"
                        | "marquee"
                        | "object"
                        | "select"
                        | "template"
                ) || (scope == Scope::ListItem && matches!(name, "ol" | "ul"))
                    || (scope == Scope::Button && name == "button")
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

#[derive(Debug)]
struct Open {
    anchor: Anchor,
    space: bool,
    segment: usize,
}

#[derive(Debug, Default)]
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
    reopen: Vec<Option<(LocalName, Own)>>,
    form_open: bool,
}

const MAX_REOPEN: usize = 32;

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

    fn start(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name = &*tag.name;
        if self.foreign_mode() {
            let font_breaks = name == "font"
                && tag
                    .attrs
                    .iter()
                    .any(|a| matches!(&*a.name.local, "color" | "face" | "size"));
            if names::is_breakout(name) || font_breaks {
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
                    self.push_foreign(tag.name.clone(), ns);
                }
                return TokenSinkResult::Continue;
            }
        }
        self.html_start(tag)
    }

    fn html_start(&mut self, tag: &Tag) -> TokenSinkResult<()> {
        let name = &*tag.name;
        match name {
            "html" | "head" | "body" | "frameset" => return TokenSinkResult::Continue,
            "svg" | "math" => {
                self.reconstruct();
                if !tag.self_closing {
                    let ns = if name == "svg" { Ns::Svg } else { Ns::Math };
                    self.push_foreign(tag.name.clone(), ns);
                }
                return TokenSinkResult::Continue;
            }
            _ => {}
        }
        let table_part = matches!(
            name,
            "caption" | "col" | "colgroup" | "tbody" | "td" | "tfoot" | "th" | "thead" | "tr"
        );
        if table_part {
            if !self.stack.iter().any(|f| f.is("table") || f.is("template")) {
                return TokenSinkResult::Continue;
            }
            self.close_in_scope(|n| matches!(n, "td" | "th"), Scope::Table);
            self.close_in_scope(|n| n == "caption", Scope::Table);
            let context: &[&str] = match name {
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
        if self.table_mode() {
            match name {
                "form" | "input" => {
                    if name == "form" && !self.stack.iter().any(|f| f.is("template")) {
                        self.form_open = true;
                    }
                    return TokenSinkResult::Continue;
                }
                "table" => self.close_in_scope(|n| n == "table", Scope::Table),
                _ => {}
            }
        }
        if name == "form" {
            if self.form_open && !self.stack.iter().any(|f| f.is("template")) {
                return TokenSinkResult::Continue;
            }
            self.form_open = true;
        }
        if name == "select" && self.try_close(|n| n == "select", Scope::Default) {
            return TokenSinkResult::Continue;
        }
        self.implicit_close(name);
        if reconstructs(name) {
            self.reconstruct();
        }
        if let Some(result) = self.raw_start(tag) {
            return result;
        }
        if names::is_void(name) {
            if matches!(name, "br" | "hr") {
                self.line_break();
            }
            return TokenSinkResult::Continue;
        }
        let frame = self.frame_for(tag);
        if frame.block {
            self.line_break();
        }
        if frame.anchor && self.stack.len() < MAX_DEPTH {
            self.open_anchor(tag, frame.ctx.hidden);
        }
        self.push(frame);
        TokenSinkResult::Continue
    }

    fn raw_start(&mut self, tag: &Tag) -> Option<TokenSinkResult<()>> {
        let name = &*tag.name;
        let kind = match name {
            "script" => RawKind::ScriptData,
            "style" | "xmp" | "iframe" | "noembed" | "noframes" | "noscript" => RawKind::Rawtext,
            "title" | "textarea" => RawKind::Rcdata,
            "plaintext" => {
                self.raw = Raw::Skip { separates: true };
                return Some(TokenSinkResult::Plaintext);
            }
            _ => return None,
        };
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
        Some(TokenSinkResult::RawData(kind))
    }

    fn frame_for(&self, tag: &Tag) -> Frame {
        let parent = self.ctx();
        let name = &*tag.name;
        let style = attribute(tag, "style")
            .map(Style::parse)
            .unwrap_or_default();
        let dropped = names::is_dropped(name);
        let hidden = attribute(tag, "hidden").is_some()
            || attribute(tag, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
            || style.hides()
            || names::skips_subtree(name)
            || dropped;
        let emphasis = matches!(name, "strong" | "b" | "h1" | "h2" | "h3")
            || style.emphasis()
            || (name == "font" && attribute(tag, "size").is_some_and(large_font_attribute));
        let cell_button = matches!(name, "td" | "th").then(|| {
            style.button()
                || attribute(tag, "bgcolor").is_some_and(|v| !v.trim().is_empty())
                || attribute(tag, "background").is_some_and(|v| !v.trim().is_empty())
        });
        let own = Own {
            hidden,
            inert: name == "template" || dropped,
            emphasis,
            cell_button,
        };
        Frame {
            name: tag.name.clone(),
            ns: Ns::Html,
            own,
            ctx: own.under(parent),
            anchor: name == "a",
            block: names::is_block(name),
        }
    }

    fn push_foreign(&mut self, name: LocalName, ns: Ns) {
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
        });
    }

    fn push(&mut self, frame: Frame) {
        if is_scope_marker(&frame) && self.stack.len() < MAX_DEPTH {
            self.reopen.push(None);
        }
        if self.stack.len() >= MAX_DEPTH {
            let parent = self.ctx();
            self.deep_inert |= frame.ctx.inert && !parent.inert;
            self.deep_hidden |= frame.ctx.hidden;
            return;
        }
        self.stack.push(frame);
    }

    fn pop(&mut self) -> Option<Frame> {
        let frame = self.stack.pop()?;
        self.deep_hidden = false;
        if frame.anchor {
            self.close_anchor();
        }
        if frame.block {
            self.line_break();
        }
        if is_scope_marker(&frame) {
            let last = self.reopen.iter().rposition(Option::is_none).unwrap_or(0);
            self.reopen.truncate(last);
        }
        Some(frame)
    }

    fn table_mode(&self) -> bool {
        for frame in self.stack.iter().rev().filter(|f| f.ns == Ns::Html) {
            match &*frame.name {
                "td" | "th" | "caption" | "template" => return false,
                "table" | "tbody" | "thead" | "tfoot" | "tr" => return true,
                _ => {}
            }
        }
        false
    }

    fn reconstruct(&mut self) {
        let from = self
            .reopen
            .iter()
            .rposition(Option::is_none)
            .map_or(0, |marker| marker + 1);
        if from >= self.reopen.len() {
            return;
        }
        for (name, own) in self.reopen.split_off(from).into_iter().flatten() {
            let ctx = own.under(self.ctx());
            self.push(Frame {
                name,
                ns: Ns::Html,
                own,
                ctx,
                anchor: false,
                block: false,
            });
        }
    }

    fn remove(&mut self, index: usize) {
        if index >= self.stack.len() {
            return;
        }
        let frame = self.stack.remove(index);
        if frame.anchor {
            self.close_anchor();
        }
        if frame.block {
            self.line_break();
        }
        let mut parent = index
            .checked_sub(1)
            .and_then(|i| self.stack.get(i))
            .map(|f| f.ctx)
            .unwrap_or_default();
        for frame in self.stack.iter_mut().skip(index) {
            frame.ctx = frame.own.under(parent);
            parent = frame.ctx;
        }
    }

    fn implied_end(&mut self) {
        while self.stack.last().is_some_and(|f| {
            f.ns == Ns::Html
                && matches!(
                    &*f.name,
                    "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc"
                )
        }) {
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
                    .is_some_and(|f| !(f.ns == Ns::Html && names::is_formatting(&f.name)))
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
        let mut closed = Vec::new();
        while self.stack.len() > index {
            let Some(frame) = self.pop() else {
                break;
            };
            if is_scope_marker(&frame) {
                closed.clear();
            }
            if self.stack.len() > index && frame.ns == Ns::Html && names::is_formatting(&frame.name)
            {
                closed.push(Some((frame.name, frame.own)));
            }
        }
        closed.reverse();
        self.reopen.extend(closed);
        let entries = self.reopen.iter().filter(|e| e.is_some()).count();
        if entries > MAX_REOPEN {
            let first = self.reopen.iter().position(Option::is_some).unwrap_or(0);
            self.reopen.remove(first);
        }
    }

    fn close_in_scope(&mut self, matches: impl Fn(&str) -> bool, scope: Scope) {
        self.try_close(matches, scope);
    }

    fn try_close(&mut self, matches: impl Fn(&str) -> bool, scope: Scope) -> bool {
        for index in (0..self.stack.len()).rev() {
            let Some(frame) = self.stack.get(index) else {
                break;
            };
            if frame.ns == Ns::Html && matches(&frame.name) {
                self.pop_to(index);
                return true;
            }
            if frame.bounds(scope) {
                break;
            }
        }
        false
    }

    fn close_list_item(&mut self, targets: &[&str]) {
        for index in (0..self.stack.len()).rev() {
            let Some(frame) = self.stack.get(index) else {
                return;
            };
            if frame.ns == Ns::Html && targets.contains(&&*frame.name) {
                self.pop_to(index);
                return;
            }
            if frame.special() && !(frame.is("address") || frame.is("div") || frame.is("p")) {
                return;
            }
        }
    }

    fn implicit_close(&mut self, name: &str) {
        if names::closes_p(name) {
            self.close_in_scope(|n| n == "p", Scope::Button);
        }
        match name {
            n if names::is_heading(n) => {
                if self
                    .stack
                    .last()
                    .is_some_and(|f| f.ns == Ns::Html && names::is_heading(&f.name))
                {
                    self.pop();
                }
            }
            "li" => self.close_list_item(&["li"]),
            "dd" | "dt" => self.close_list_item(&["dd", "dt"]),
            "a" => {
                let mut in_scope = true;
                for index in (0..self.stack.len()).rev() {
                    let Some(frame) = self.stack.get(index) else {
                        break;
                    };
                    if frame.is("a") {
                        if in_scope {
                            self.adopt(index);
                        }
                        if self.stack.get(index).is_some_and(|f| f.is("a")) {
                            self.remove(index);
                        }
                        break;
                    }
                    in_scope &= !frame.bounds(Scope::Default);
                    if [
                        "td", "th", "caption", "template", "applet", "object", "marquee",
                    ]
                    .iter()
                    .any(|marker| frame.is(marker))
                    {
                        break;
                    }
                }
            }
            "td" | "th" => self.close_in_scope(|n| matches!(n, "td" | "th"), Scope::Table),
            "tr" => {
                self.close_in_scope(|n| matches!(n, "td" | "th"), Scope::Table);
                self.close_in_scope(|n| n == "tr", Scope::Table);
            }
            "tbody" | "thead" | "tfoot" => {
                self.close_in_scope(|n| matches!(n, "td" | "th"), Scope::Table);
                self.close_in_scope(|n| n == "tr", Scope::Table);
                self.close_in_scope(|n| matches!(n, "tbody" | "thead" | "tfoot"), Scope::Table);
            }
            "option" => {
                if self.stack.last().is_some_and(|f| f.is("option")) {
                    self.pop();
                }
            }
            "optgroup" => {
                if self.stack.last().is_some_and(|f| f.is("option")) {
                    self.pop();
                }
                if self.stack.last().is_some_and(|f| f.is("optgroup")) {
                    self.pop();
                }
            }
            "button" => self.close_in_scope(|n| n == "button", Scope::Default),
            "input" => self.close_in_scope(|n| n == "select", Scope::Default),
            _ => {}
        }
    }

    fn end(&mut self, name: &LocalName) {
        if !matches!(self.raw, Raw::None) {
            self.end_raw();
            return;
        }
        let name = &**name;
        if self.stack.last().is_some_and(|frame| frame.ns != Ns::Html) {
            if matches!(name, "br" | "p") {
                while self
                    .stack
                    .last()
                    .is_some_and(|frame| frame.ns != Ns::Html && !frame.integration())
                {
                    self.pop();
                }
            } else {
                for index in (0..self.stack.len()).rev() {
                    let Some(frame) = self.stack.get(index) else {
                        return;
                    };
                    if frame.ns == Ns::Html {
                        break;
                    }
                    if (*frame.name).eq_ignore_ascii_case(name) {
                        self.pop_to(index);
                        return;
                    }
                }
            }
        }
        self.html_end(name);
    }

    fn html_end(&mut self, name: &str) {
        match name {
            "br" => self.line_break(),
            "html" | "head" | "body" | "frameset" => {}
            "p" => {
                if !self.try_close(|n| n == "p", Scope::Button) {
                    self.line_break();
                }
            }
            "li" => self.close_in_scope(|n| n == "li", Scope::ListItem),
            n if names::is_heading(n) => self.close_in_scope(names::is_heading, Scope::Default),
            "tr" | "tbody" | "thead" | "tfoot" => {
                let _ = self.try_close(|n| n == name, Scope::Table)
                    || self.try_close(|n| n == "tr", Scope::Table)
                    || self.try_close(|n| matches!(n, "td" | "th"), Scope::Table);
            }
            "td" | "th" | "table" | "caption" | "colgroup" => {
                self.close_in_scope(|n| n == name, Scope::Table);
            }
            "form" => {
                self.form_open = false;
                for index in (0..self.stack.len()).rev() {
                    let Some(frame) = self.stack.get(index) else {
                        return;
                    };
                    if frame.is("form") {
                        if self.stack.iter().any(|f| f.is("template")) {
                            self.pop_to(index);
                        } else {
                            self.implied_end();
                            self.remove(index);
                        }
                        return;
                    }
                    if frame.bounds(Scope::Default) {
                        return;
                    }
                }
            }
            "template" => {
                if let Some(index) = self.stack.iter().rposition(|f| f.is("template")) {
                    self.pop_to(index);
                }
            }
            n if names::is_formatting(n) => {
                let marker = self.reopen.iter().rposition(Option::is_none);
                if let Some(entry) = self
                    .reopen
                    .iter()
                    .rposition(|e| e.as_ref().is_some_and(|(n, _)| &**n == name))
                    .filter(|entry| marker.is_none_or(|m| *entry > m))
                {
                    self.reopen.remove(entry);
                    return;
                }
                for index in (0..self.stack.len()).rev() {
                    let Some(frame) = self.stack.get(index) else {
                        return;
                    };
                    if frame.is(name) {
                        self.adopt(index);
                        return;
                    }
                    if frame.bounds(Scope::Default) {
                        return;
                    }
                }
            }
            n if names::is_special(n) => self.close_in_scope(|n| n == name, Scope::Default),
            _ => {
                for index in (0..self.stack.len()).rev() {
                    let Some(frame) = self.stack.get(index) else {
                        return;
                    };
                    if frame.is(name) {
                        self.pop_to(index);
                        return;
                    }
                    if frame.special() {
                        return;
                    }
                }
            }
        }
    }

    fn end_raw(&mut self) {
        if let Raw::LdJson(block, false) = mem::take(&mut self.raw)
            && self.ld_json.len() < MAX_LD_JSON_BLOCKS
            && !block.trim().is_empty()
        {
            self.ld_json.push(block);
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
        let forward = self.forward.map(|(_, anchor)| anchor);
        let footer = self.footer.map(|(_, anchor)| anchor);
        for anchor in &mut self.anchors {
            anchor.below_forward = forward.is_some_and(|from| anchor.index >= from);
            anchor.in_footer = footer.is_some_and(|from| anchor.index >= from);
        }
        HtmlView::new()
            .with_anchors(self.anchors)
            .with_ld_json(self.ld_json)
            .with_segments(self.segments)
            .with_footer_at(self.footer.map(|(segment, _)| segment))
    }
}

fn is_scope_marker(frame: &Frame) -> bool {
    frame.ns == Ns::Html
        && matches!(
            &*frame.name,
            "td" | "th" | "caption" | "template" | "applet" | "object" | "marquee"
        )
}

fn reconstructs(name: &str) -> bool {
    !(names::closes_p(name)
        || matches!(
            name,
            "form"
                | "caption"
                | "col"
                | "colgroup"
                | "tbody"
                | "td"
                | "tfoot"
                | "th"
                | "thead"
                | "tr"
                | "template"
                | "script"
                | "style"
                | "title"
                | "textarea"
                | "noscript"
                | "noframes"
                | "option"
                | "optgroup"
        ))
}

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

fn collapse(out: &mut String, space: &mut bool, text: &str) {
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
