use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe, PanicHookInfo};
use std::sync::Once;

use holtmail_insight::{Insight, InsightExtractor, InsightInput};

#[derive(Clone)]
struct Extraction {
    id: String,
    version: String,
}

thread_local! {
    static IN_EXTRACTION: RefCell<Option<Extraction>> = const { RefCell::new(None) };
    static MESSAGE: RefCell<Option<String>> = const { RefCell::new(None) };
    #[cfg(test)]
    static CAPTURE: RefCell<Option<Vec<String>>> = const { RefCell::new(None) };
}

struct Restore<T: 'static> {
    key: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    previous: Option<T>,
}

impl<T: 'static> Drop for Restore<T> {
    fn drop(&mut self) {
        let previous = self.previous.take();
        self.key.with(|cell| *cell.borrow_mut() = previous);
    }
}

fn set<T: 'static>(
    key: &'static std::thread::LocalKey<RefCell<Option<T>>>,
    value: T,
) -> Restore<T> {
    let previous = key.with(|cell| cell.borrow_mut().replace(value));
    Restore { key, previous }
}

/// Runs `f` with `message` as the message id that panic reports of extractors on this thread
/// name. The previous id is restored when `f` returns or unwinds. Nests.
pub fn with_message<R>(message: impl ToString, f: impl FnOnce() -> R) -> R {
    let _restore = set(&MESSAGE, message.to_string());
    f()
}

pub(crate) fn run_extractor(
    extractor: &dyn InsightExtractor,
    version: &str,
    input: &InsightInput,
) -> Option<Vec<Insight>> {
    scoped(extractor, version, || extractor.extract(input))
}

/// `f` inside the extraction scope of `extractor`; `None` when it panicked.
pub(crate) fn scoped<R>(
    extractor: &dyn InsightExtractor,
    version: &str,
    f: impl FnOnce() -> R,
) -> Option<R> {
    let _restore = set(
        &IN_EXTRACTION,
        Extraction {
            id: extractor.id().to_owned(),
            version: version.to_owned(),
        },
    );
    panic::catch_unwind(AssertUnwindSafe(f)).ok()
}

/// Installs the process panic hook that keeps extractor panics free of message content.
///
/// A panic inside an extractor run by [`Runner`](crate::Runner) is reported as one error event
/// naming only the extractor id, its version, the message id set by [`with_message`] and the
/// source `file:line`; the panic payload, which may quote message content, is never printed.
/// Every other panic goes to the hook that was installed before. Installing more than once has
/// no further effect.
pub fn install_panic_hook() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| match report(info) {
            Some(line) => emit(line),
            None => previous(info),
        }));
    });
}

fn report(info: &PanicHookInfo<'_>) -> Option<String> {
    let extraction = IN_EXTRACTION
        .try_with(|cell| cell.try_borrow().ok().and_then(|e| e.clone()))
        .ok()
        .flatten()?;
    let message = MESSAGE
        .try_with(|cell| cell.try_borrow().ok().and_then(|m| m.clone()))
        .ok()
        .flatten()
        .unwrap_or_else(|| "-".to_owned());
    let location = info
        .location()
        .map(|l| format!("{}:{}", l.file(), l.line()))
        .unwrap_or_else(|| "-".to_owned());
    Some(format!(
        "insight extractor panicked: extractor={} version={} message={} at {}",
        extraction.id, extraction.version, message, location
    ))
}

fn emit(line: String) {
    #[cfg(test)]
    {
        let captured = CAPTURE.with(|cell| {
            cell.borrow_mut().as_mut().map(|lines| {
                lines.push(line.clone());
            })
        });
        if captured.is_some() {
            return;
        }
    }
    tracing::error!("{line}");
}

#[cfg(test)]
pub(crate) fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    let _restore = set(&CAPTURE, Vec::new());
    let result = f();
    let lines = CAPTURE
        .with(|cell| cell.borrow_mut().take())
        .unwrap_or_default();
    (result, lines)
}
