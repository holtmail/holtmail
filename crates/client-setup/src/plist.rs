use crate::xml::escape;

pub(crate) enum Value {
    String(String),
    Integer(u64),
    Bool(bool),
    Array(Vec<Value>),
    Dict(Vec<(&'static str, Value)>),
}

impl Value {
    pub(crate) fn string(text: impl Into<String>) -> Self {
        Self::String(text.into())
    }
}

pub(crate) fn document(root: &Value) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n",
    );
    write(&mut out, root, 0);
    out.push_str("</plist>\n");
    out
}

fn write(out: &mut String, value: &Value, depth: usize) {
    let indent = "\t".repeat(depth);
    match value {
        Value::String(text) => {
            out.push_str(&format!("{indent}<string>{}</string>\n", escape(text)))
        }
        Value::Integer(n) => out.push_str(&format!("{indent}<integer>{n}</integer>\n")),
        Value::Bool(true) => out.push_str(&format!("{indent}<true/>\n")),
        Value::Bool(false) => out.push_str(&format!("{indent}<false/>\n")),
        Value::Array(items) => {
            out.push_str(&format!("{indent}<array>\n"));
            for item in items {
                write(out, item, depth + 1);
            }
            out.push_str(&format!("{indent}</array>\n"));
        }
        Value::Dict(entries) => {
            out.push_str(&format!("{indent}<dict>\n"));
            for (key, item) in entries {
                out.push_str(&format!("{indent}\t<key>{}</key>\n", escape(key)));
                write(out, item, depth + 1);
            }
            out.push_str(&format!("{indent}</dict>\n"));
        }
    }
}
