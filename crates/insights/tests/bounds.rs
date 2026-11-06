//! Memory and time bounds of stage A on hostile input (docs/INSIGHTS.md §13): no single
//! allocation over 32 MiB anywhere, and in release builds at most 40 ms for the extractors, merge
//! and validation per message at every cap. Building the input (MIME text, NFKC, the HTML scan) is
//! timed and printed, not asserted.

#![allow(unsafe_code)]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use holtmail_insight::InsightInput;
use holtmail_insight::testing::hostile_corpus;
use holtmail_insights::Runner;
use holtmail_mime::ParsedMessage;

const MAX_ALLOCATION: usize = 32 * 1024 * 1024;
const MAX_TIME: Duration = Duration::from_millis(40);

struct Counting;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note(size: usize) {
    if WATCHING.try_with(Cell::get).unwrap_or(false) {
        let _ = LARGEST.try_with(|largest| largest.set(largest.get().max(size)));
    }
}

// SAFETY: every call is forwarded unchanged to the system allocator.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The largest single allocation `f` makes on this thread, and how long it took.
fn watch<R>(f: impl FnOnce() -> R) -> (R, usize, Duration) {
    LARGEST.with(|largest| largest.set(0));
    WATCHING.with(|watching| watching.set(true));
    let started = Instant::now();
    let result = f();
    let took = started.elapsed();
    WATCHING.with(|watching| watching.set(false));
    (result, LARGEST.with(Cell::get), took)
}

fn messages() -> Vec<(&'static str, ParsedMessage)> {
    let head = "From: Example <no-reply@accounts.example.com>\r\nTo: ada@example.org\r\n\
                Subject: Your verification code\r\nMIME-Version: 1.0\r\n";
    let mut out = Vec::new();
    let mut text = format!("{head}Content-Type: text/plain; charset=utf-8\r\n\r\nYour code is ");
    text.push_str(&"7".repeat(8 * 1024 * 1024));
    out.push(("text-8mib-one-token", text));

    let mut lines = format!("{head}Content-Type: text/plain; charset=utf-8\r\n\r\n");
    for n in 0..200_000u32 {
        lines.push_str(&format!("code {:06}\r\n", n * 7 % 1_000_000));
    }
    out.push(("text-200k-codes", lines));

    let mut nested = format!("{head}Content-Type: text/html; charset=utf-8\r\n\r\n");
    nested.push_str(&"<div><b>".repeat(20_000));
    nested.push_str("Your code 482913");
    out.push(("html-40k-nested-tags", nested));

    let mut anchors = format!("{head}Content-Type: text/html; charset=utf-8\r\n\r\n");
    for n in 0..20_000 {
        anchors.push_str(&format!(
            "<a href=\"https://example.com/v?c={n}\">verify {n}</a> <strong>{n:06}</strong>\n"
        ));
    }
    out.push(("html-20k-anchors-and-codes", anchors));

    let mut combining = format!("{head}Content-Type: text/plain; charset=utf-8\r\n\r\ncode ");
    combining.push('a');
    combining.push_str(&"\u{0301}".repeat(1024 * 1024));
    out.push(("text-1m-combining-marks", combining));

    let mut fields = String::new();
    for n in 0..2_000 {
        fields.push_str(&format!(
            "List-Unsubscribe: <https://news.example.com/u/{n}>\r\nDKIM-Signature: v=1; d=example.com; s=s{n}; h=from:list-unsubscribe; b=AAAA{n}\r\n"
        ));
    }
    out.push((
        "headers-4k-fields",
        format!("{head}{fields}Content-Type: text/plain\r\n\r\nYour code 482913\r\n"),
    ));

    out.into_iter()
        .map(|(name, raw)| {
            let parsed = holtmail_mime::parse(raw.as_bytes())
                .unwrap_or_else(|_| panic!("{name} does not parse"));
            (name, parsed)
        })
        .collect()
}

fn check(name: &str, largest: usize, took: Duration) {
    println!("{name}: largest allocation {largest} bytes, {took:?}");
    assert!(
        largest <= MAX_ALLOCATION,
        "{name}: one allocation of {largest} bytes"
    );
    if !cfg!(debug_assertions) {
        assert!(took <= MAX_TIME, "{name}: {took:?}");
    }
}

fn fastest<R>(mut f: impl FnMut() -> R) -> (R, usize, Duration) {
    let runs = if cfg!(debug_assertions) { 1 } else { 3 };
    let (mut result, mut largest, mut best) = watch(&mut f);
    for _ in 1..runs {
        let (again, size, took) = watch(&mut f);
        result = again;
        largest = largest.max(size);
        best = best.min(took);
    }
    (result, largest, best)
}

#[test]
fn hostile_inputs_stay_within_bounds() {
    let runner = common::runner();
    for case in hostile_corpus() {
        let input: &InsightInput = &case.input;
        let (_, largest, took) = fastest(|| runner.extract_input(input, false));
        check(&case.name, largest, took);
    }
}

#[test]
fn hostile_messages_stay_within_bounds() {
    let runner = common::runner();
    for (name, message) in messages() {
        let ((input, truncated), input_largest, building) = fastest(|| Runner::input(&message));
        println!("{name}: building the input took {building:?}");
        let (_, largest, took) = fastest(|| runner.extract_input(&input, truncated));
        check(name, largest.max(input_largest), took);
    }
}

#[test]
fn the_counter_sees_large_allocations() {
    let (_, largest, _) = watch(|| std::hint::black_box(vec![0u8; 40 * 1024 * 1024]));
    assert!(largest > MAX_ALLOCATION);
}
