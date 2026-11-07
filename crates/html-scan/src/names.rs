/// Every name class the scan asks about, computed once per tag name.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Kind(u32);

impl Kind {
    pub(crate) const VOID: u32 = 1;
    pub(crate) const BLOCK: u32 = 1 << 1;
    pub(crate) const HEADING: u32 = 1 << 2;
    pub(crate) const CLOSES_P: u32 = 1 << 3;
    pub(crate) const SPECIAL: u32 = 1 << 4;
    pub(crate) const FORMATTING: u32 = 1 << 5;
    pub(crate) const BREAKOUT: u32 = 1 << 6;
    pub(crate) const SVG_INTEGRATION: u32 = 1 << 7;
    pub(crate) const MATH_INTEGRATION: u32 = 1 << 8;
    pub(crate) const SKIPS_SUBTREE: u32 = 1 << 9;
    pub(crate) const DROPPED: u32 = 1 << 10;
    pub(crate) const NO_RECONSTRUCT: u32 = 1 << 11;
    pub(crate) const TABLE_PART: u32 = 1 << 12;
    pub(crate) const MARKER: u32 = 1 << 13;
    pub(crate) const BOUND: u32 = 1 << 14;
    pub(crate) const BOUND_LIST: u32 = 1 << 15;
    pub(crate) const BOUND_BUTTON: u32 = 1 << 16;
    pub(crate) const BOUND_TABLE: u32 = 1 << 17;
    pub(crate) const IMPLIED_END: u32 = 1 << 18;
    pub(crate) const EMPHASIS: u32 = 1 << 19;
    pub(crate) const RAW: u32 = 1 << 20;
    pub(crate) const STRUCTURE: u32 = 1 << 21;
    pub(crate) const FOREIGN_ROOT: u32 = 1 << 22;
    pub(crate) const TABLE_MODE_OFF: u32 = 1 << 23;
    pub(crate) const TABLE_MODE_ON: u32 = 1 << 24;
    pub(crate) const IMPLICIT_CLOSE: u32 = 1 << 25;
    pub(crate) const END_RULE: u32 = 1 << 26;
    pub(crate) const LIST_ITEM_PASS: u32 = 1 << 27;
    pub(crate) const CELL: u32 = 1 << 28;

    pub(crate) fn of(name: &str) -> Kind {
        let rules: [(u32, bool); 29] = [
            (Kind::VOID, is_void(name)),
            (Kind::BLOCK, is_block(name)),
            (Kind::HEADING, is_heading(name)),
            (Kind::CLOSES_P, closes_p(name)),
            (Kind::SPECIAL, is_special(name)),
            (Kind::FORMATTING, is_formatting(name)),
            (Kind::BREAKOUT, is_breakout(name)),
            (Kind::SVG_INTEGRATION, is_svg_integration(name)),
            (Kind::MATH_INTEGRATION, is_math_integration(name)),
            (Kind::SKIPS_SUBTREE, skips_subtree(name)),
            (Kind::DROPPED, is_dropped(name)),
            (
                Kind::NO_RECONSTRUCT,
                closes_p(name)
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
                    ),
            ),
            (
                Kind::TABLE_PART,
                matches!(
                    name,
                    "caption"
                        | "col"
                        | "colgroup"
                        | "tbody"
                        | "td"
                        | "tfoot"
                        | "th"
                        | "thead"
                        | "tr"
                ),
            ),
            (
                Kind::MARKER,
                matches!(
                    name,
                    "td" | "th" | "caption" | "template" | "applet" | "object" | "marquee"
                ),
            ),
            (
                Kind::BOUND,
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
                ),
            ),
            (Kind::BOUND_LIST, matches!(name, "ol" | "ul")),
            (Kind::BOUND_BUTTON, name == "button"),
            (
                Kind::BOUND_TABLE,
                matches!(name, "html" | "table" | "template"),
            ),
            (
                Kind::IMPLIED_END,
                matches!(
                    name,
                    "dd" | "dt" | "li" | "optgroup" | "option" | "p" | "rb" | "rp" | "rt" | "rtc"
                ),
            ),
            (
                Kind::EMPHASIS,
                matches!(name, "strong" | "b" | "h1" | "h2" | "h3"),
            ),
            (
                Kind::RAW,
                matches!(
                    name,
                    "script"
                        | "style"
                        | "xmp"
                        | "iframe"
                        | "noembed"
                        | "noframes"
                        | "noscript"
                        | "title"
                        | "textarea"
                        | "plaintext"
                ),
            ),
            (
                Kind::STRUCTURE,
                matches!(name, "html" | "head" | "body" | "frameset"),
            ),
            (Kind::FOREIGN_ROOT, matches!(name, "svg" | "math")),
            (
                Kind::TABLE_MODE_OFF,
                matches!(name, "td" | "th" | "caption" | "template"),
            ),
            (
                Kind::TABLE_MODE_ON,
                matches!(name, "table" | "tbody" | "thead" | "tfoot" | "tr"),
            ),
            (
                Kind::IMPLICIT_CLOSE,
                is_heading(name)
                    || matches!(
                        name,
                        "li" | "dd"
                            | "dt"
                            | "a"
                            | "td"
                            | "th"
                            | "tr"
                            | "tbody"
                            | "thead"
                            | "tfoot"
                            | "option"
                            | "optgroup"
                            | "button"
                            | "input"
                    ),
            ),
            (
                Kind::END_RULE,
                is_heading(name)
                    || matches!(
                        name,
                        "br" | "html"
                            | "head"
                            | "body"
                            | "frameset"
                            | "p"
                            | "li"
                            | "tr"
                            | "tbody"
                            | "thead"
                            | "tfoot"
                            | "td"
                            | "th"
                            | "table"
                            | "caption"
                            | "colgroup"
                            | "form"
                            | "template"
                    ),
            ),
            (
                Kind::LIST_ITEM_PASS,
                matches!(name, "address" | "div" | "p"),
            ),
            (Kind::CELL, matches!(name, "td" | "th")),
        ];
        Kind(
            rules
                .iter()
                .filter(|(_, on)| *on)
                .fold(0, |bits, (bit, _)| bits | bit),
        )
    }

    pub(crate) fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
}

pub(crate) fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area"
            | "base"
            | "basefont"
            | "bgsound"
            | "br"
            | "col"
            | "embed"
            | "frame"
            | "hr"
            | "image"
            | "img"
            | "input"
            | "keygen"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
    )
}

pub(crate) fn is_block(name: &str) -> bool {
    is_heading(name)
        || matches!(
            name,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "caption"
                | "center"
                | "dd"
                | "details"
                | "dialog"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "header"
                | "hgroup"
                | "li"
                | "listing"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "option"
                | "p"
                | "pre"
                | "search"
                | "section"
                | "summary"
                | "table"
                | "tbody"
                | "td"
                | "tfoot"
                | "th"
                | "thead"
                | "tr"
                | "ul"
        )
}

pub(crate) fn is_heading(name: &str) -> bool {
    matches!(name, "h1" | "h2" | "h3" | "h4" | "h5" | "h6")
}

pub(crate) fn closes_p(name: &str) -> bool {
    is_heading(name)
        || matches!(
            name,
            "address"
                | "article"
                | "aside"
                | "blockquote"
                | "center"
                | "dd"
                | "details"
                | "dialog"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "header"
                | "hgroup"
                | "hr"
                | "li"
                | "listing"
                | "main"
                | "menu"
                | "nav"
                | "ol"
                | "p"
                | "plaintext"
                | "pre"
                | "search"
                | "section"
                | "summary"
                | "table"
                | "ul"
                | "xmp"
        )
}

pub(crate) fn is_special(name: &str) -> bool {
    is_heading(name)
        || matches!(
            name,
            "address"
                | "applet"
                | "area"
                | "article"
                | "aside"
                | "base"
                | "basefont"
                | "bgsound"
                | "blockquote"
                | "body"
                | "br"
                | "button"
                | "caption"
                | "center"
                | "col"
                | "colgroup"
                | "dd"
                | "details"
                | "dir"
                | "div"
                | "dl"
                | "dt"
                | "embed"
                | "fieldset"
                | "figcaption"
                | "figure"
                | "footer"
                | "form"
                | "frame"
                | "frameset"
                | "head"
                | "header"
                | "hgroup"
                | "hr"
                | "html"
                | "iframe"
                | "img"
                | "input"
                | "keygen"
                | "li"
                | "link"
                | "listing"
                | "main"
                | "marquee"
                | "menu"
                | "meta"
                | "nav"
                | "noembed"
                | "noframes"
                | "noscript"
                | "object"
                | "ol"
                | "p"
                | "param"
                | "plaintext"
                | "pre"
                | "script"
                | "search"
                | "section"
                | "select"
                | "source"
                | "style"
                | "summary"
                | "table"
                | "tbody"
                | "td"
                | "template"
                | "textarea"
                | "tfoot"
                | "th"
                | "thead"
                | "title"
                | "tr"
                | "track"
                | "ul"
                | "wbr"
                | "xmp"
        )
}

pub(crate) fn is_formatting(name: &str) -> bool {
    matches!(
        name,
        "a" | "b"
            | "big"
            | "code"
            | "em"
            | "font"
            | "i"
            | "nobr"
            | "s"
            | "small"
            | "strike"
            | "strong"
            | "tt"
            | "u"
    )
}

pub(crate) fn is_breakout(name: &str) -> bool {
    is_heading(name)
        || matches!(
            name,
            "b" | "big"
                | "blockquote"
                | "body"
                | "br"
                | "center"
                | "code"
                | "dd"
                | "div"
                | "dl"
                | "dt"
                | "em"
                | "embed"
                | "head"
                | "hr"
                | "i"
                | "img"
                | "li"
                | "listing"
                | "menu"
                | "meta"
                | "nobr"
                | "ol"
                | "p"
                | "pre"
                | "ruby"
                | "s"
                | "small"
                | "span"
                | "strong"
                | "strike"
                | "sub"
                | "sup"
                | "table"
                | "tt"
                | "u"
                | "ul"
                | "var"
        )
}

pub(crate) fn is_svg_integration(name: &str) -> bool {
    matches!(name, "foreignobject" | "desc" | "title")
}

pub(crate) fn is_math_integration(name: &str) -> bool {
    matches!(name, "mi" | "mo" | "mn" | "ms" | "mtext" | "annotation-xml")
}

pub(crate) fn skips_subtree(name: &str) -> bool {
    matches!(
        name,
        "template" | "object" | "applet" | "select" | "datalist"
    )
}

/// HTML elements named like an SVG or MathML element: the sanitizer drops them with their content.
pub(crate) fn is_dropped(name: &str) -> bool {
    DROPPED.binary_search(&name).is_ok()
}

const DROPPED: &[&str] = &[
    "abs",
    "and",
    "animate",
    "annotation",
    "annotation-xml",
    "apply",
    "approx",
    "arccos",
    "arccosh",
    "arccot",
    "arccoth",
    "arccsc",
    "arccsch",
    "arcsec",
    "arcsech",
    "arcsin",
    "arcsinh",
    "arctan",
    "arctanh",
    "arg",
    "bind",
    "bvar",
    "card",
    "cartesianproduct",
    "cbytes",
    "ceiling",
    "cerror",
    "ci",
    "circle",
    "cn",
    "codomain",
    "complexes",
    "compose",
    "condition",
    "conjugate",
    "cos",
    "cosh",
    "cot",
    "coth",
    "cs",
    "csc",
    "csch",
    "csymbol",
    "curl",
    "declare",
    "defs",
    "degree",
    "desc",
    "determinant",
    "diff",
    "discard",
    "divergence",
    "divide",
    "domain",
    "domainofapplication",
    "ellipse",
    "emptyset",
    "eq",
    "equivalent",
    "eulergamma",
    "exists",
    "exp",
    "exponentiale",
    "factorial",
    "factorof",
    "false",
    "filter",
    "floor",
    "fn",
    "forall",
    "g",
    "gcd",
    "geq",
    "grad",
    "gt",
    "ident",
    "imaginary",
    "imaginaryi",
    "implies",
    "in",
    "infinity",
    "int",
    "integers",
    "intersect",
    "interval",
    "inverse",
    "lambda",
    "laplacian",
    "lcm",
    "leq",
    "limit",
    "line",
    "list",
    "ln",
    "log",
    "logbase",
    "lowlimit",
    "lt",
    "maction",
    "maligngroup",
    "malignmark",
    "marker",
    "mask",
    "math",
    "matrix",
    "matrixrow",
    "max",
    "mean",
    "median",
    "menclose",
    "merror",
    "metadata",
    "mfenced",
    "mfrac",
    "mglyph",
    "mi",
    "min",
    "minus",
    "mlabeledtr",
    "mlongdiv",
    "mmultiscripts",
    "mn",
    "mo",
    "mode",
    "moment",
    "momentabout",
    "mover",
    "mpadded",
    "mpath",
    "mphantom",
    "mprescripts",
    "mroot",
    "mrow",
    "ms",
    "mscarries",
    "mscarry",
    "msgroup",
    "msline",
    "mspace",
    "msqrt",
    "msrow",
    "mstack",
    "mstyle",
    "msub",
    "msubsup",
    "msup",
    "mtable",
    "mtd",
    "mtext",
    "mtr",
    "munder",
    "munderover",
    "naturalnumbers",
    "neq",
    "none",
    "not",
    "notanumber",
    "notin",
    "notprsubset",
    "notsubset",
    "or",
    "otherwise",
    "outerproduct",
    "partialdiff",
    "path",
    "pattern",
    "pi",
    "piece",
    "piecewise",
    "plus",
    "polygon",
    "polyline",
    "power",
    "primes",
    "product",
    "prsubset",
    "quotient",
    "rationals",
    "real",
    "reals",
    "rect",
    "reln",
    "rem",
    "root",
    "scalarproduct",
    "sdev",
    "sec",
    "sech",
    "selector",
    "semantics",
    "sep",
    "set",
    "setdiff",
    "share",
    "sin",
    "sinh",
    "stop",
    "subset",
    "sum",
    "svg",
    "switch",
    "symbol",
    "tan",
    "tanh",
    "tendsto",
    "text",
    "times",
    "transpose",
    "true",
    "tspan",
    "union",
    "uplimit",
    "use",
    "variance",
    "vector",
    "vectorproduct",
    "view",
    "xor",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropped_list_is_sorted() {
        assert!(DROPPED.windows(2).all(|w| w.first() < w.get(1)));
        assert!(is_dropped("text"));
        assert!(!is_dropped("span"));
        assert!(!is_dropped("a"));
        assert!(!is_dropped("image"));
    }
}
