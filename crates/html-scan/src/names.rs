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
