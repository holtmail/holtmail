//! The template sandbox.

use std::io;

use minijinja::{AutoEscape, Environment, ErrorKind, UndefinedBehavior};
use serde::Serialize;

/// The most bytes a template's source may have.
pub const MAX_SOURCE: usize = 16 * 1024;
/// The most bytes a rendered template may have.
pub const MAX_OUTPUT: usize = 64 * 1024;
/// Engine fuel per render: bounds the time a template can take.
pub const FUEL: u64 = 50_000;
/// The deepest nesting a template may reach.
pub const MAX_DEPTH: usize = 16;

/// Why a template was refused or failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The source is over [`MAX_SOURCE`].
    #[error("template is longer than {MAX_SOURCE} bytes")]
    SourceTooLarge,
    /// The source does not parse, or uses something the sandbox lacks (`include`,
    /// `extends`, `import`, macros).
    #[error("template syntax: {0}")]
    Syntax(String),
    /// The output would pass [`MAX_OUTPUT`].
    #[error("rendered text is longer than {MAX_OUTPUT} bytes")]
    OutputTooLarge,
    /// The template ran out of fuel.
    #[error("template took too long")]
    TooSlow,
    /// Rendering failed otherwise.
    #[error("template failed: {0}")]
    Render(String),
}

fn environment() -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(FUEL));
    env.set_recursion_limit(MAX_DEPTH);
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.set_formatter(|out, state, value| {
        if value.is_none() {
            Ok(())
        } else {
            minijinja::escape_formatter(out, state, value)
        }
    });
    env
}

/// Checks that a template parses in the sandbox, without rendering it.
pub fn check(source: &str) -> Result<(), TemplateError> {
    if source.len() > MAX_SOURCE {
        return Err(TemplateError::SourceTooLarge);
    }
    environment()
        .template_from_str(source)
        .map(|_| ())
        .map_err(|e| TemplateError::Syntax(e.to_string()))
}

/// Renders a template against a context in the sandbox.
///
/// The sandbox has no loader, no includes, no macros and no functions that reach the
/// filesystem or the network; it sees only `context`. Work is bounded by [`FUEL`] and
/// [`MAX_DEPTH`], output by [`MAX_OUTPUT`]. Undefined and missing (`none`) values print as empty
/// text. The same
/// source and context always give the same result.
pub fn render<C: Serialize>(source: &str, context: &C) -> Result<String, TemplateError> {
    check(source)?;
    let env = environment();
    let template = env
        .template_from_str(source)
        .map_err(|e| TemplateError::Syntax(e.to_string()))?;
    let ctx = minijinja::Value::from(minijinja::value::Serde(context));
    let mut out = Capped::default();
    match template.render_captured_to(ctx, &mut out) {
        Ok(_) => String::from_utf8(out.buf).map_err(|e| TemplateError::Render(e.to_string())),
        Err(_) if out.over => Err(TemplateError::OutputTooLarge),
        Err(e) if e.kind() == ErrorKind::OutOfFuel => Err(TemplateError::TooSlow),
        Err(e) => Err(TemplateError::Render(e.to_string())),
    }
}

#[derive(Default)]
struct Capped {
    buf: Vec<u8>,
    over: bool,
}

impl io::Write for Capped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.buf.len() + buf.len() > MAX_OUTPUT {
            self.over = true;
            return Err(io::Error::other("output limit"));
        }
        self.buf.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
