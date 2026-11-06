# holtmail-json-http

A small library for JSON over HTTPS with the APIs of outside services, used by the alias directories. One base address, fixed headers, a timeout, a size limit, and status codes turned into errors. Not a contract: provider crates map `HttpError` into their own contract's error.

## API

- `JsonHttp::new(base, headers, timeout, roots)`: `Invalid` for an address `allowed` refuses (anything but `https`, or `http` to a loopback address), a header that cannot be sent, or an extra root that is not a certificate. Header values are marked sensitive. No redirects, no cookies, no proxy.
- `get::<T>(path)` and `call::<T, B>(method, path, body)`: `path` is relative to the base (a trailing `/` is added to the base); an empty path is the base exactly as given. A body is sent as JSON. An empty answer reads as `null`, so `T = ()` accepts `204`.
- `HttpError`: `Invalid` (nothing sent), `Status { status, message }` with the `error`, `message`, `detail` or first `errors` text of a JSON answer (at most 300 characters, control characters dropped), `Unreachable` (no answer or a timeout), `Unexpected` (not the JSON expected, or over `MAX_ANSWER`, 16 MiB).
- `utc_millis(text)`: Unix milliseconds of `2025-03-03 18:00:00`, `2025-03-03T18:00:00Z` or `2025-03-03T18:00:00.123456+02:00`; no zone means UTC.

Messages name the host, never the path, the key or the answer.

## Tests

Against an in-process `axum` server on `127.0.0.1`: headers and bodies, status mapping (including Laravel's `errors`), `204`, non-JSON answers, an unreachable port, and the date-time forms.
