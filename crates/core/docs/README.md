# holtmail-core

Small types that several contracts share, so that ids and times mean the same thing in the store, the index, the API and the web app, and the extra TLS roots every outbound client takes.

## Ids

`ArchiveId`, `AccountId`, `FolderId`, `MessageId`, `ThreadId`, `UserId`, and for contacts and calendars `CollectionId` (an address book or a calendar) `ItemId` (a contact, event or task with all its versions) and `PersonOverrideId` (a split or join of people made by hand).

- UUIDv7: ids created later sort after ids created earlier, so lists ordered by id are ordered by creation.
- `new()` creates an id without asking the database, which keeps every store implementation free to pick its storage.
- Displays, parses (`FromStr`, `InvalidId` on failure) and serializes as the hyphenated lowercase UUID string.
- Each id is its own type; a `FolderId` cannot be passed where a `MessageId` is expected.

`PersonId` names a person derived from contacts and mail (see [`holtmail-people`](../../people/docs/README.md)). It is not created but computed: `PersonId::from_hash(bytes)` makes a UUIDv8 from 16 bytes of a hash, so the same group of contacts and addresses gets the same id in every rebuild. It displays, parses and serializes like the others.

`EventGroupId` names the copies of one calendar event in an archive: every event item with the same iCalendar `UID` (see [`holtmail-calendar`](../../calendar/docs/README.md)). Like `PersonId` it is computed with `EventGroupId::from_hash(bytes)`, so the same archive and `UID` always give the same id.

## Timestamp

Milliseconds since the Unix epoch, UTC, as `i64`. Serializes as a plain integer. `now()`, `from_millis`, `from_secs`, `as_millis`, `as_secs` (rounds down, also for times before 1970). Formatting for people happens at the edges, in the web app.

## Address

`Address { name: Option<String>, email: String }`: one mailbox from an address header, as parsed by `holtmail-mime` and stored by `holtmail-store`.

## TlsRoots

Extra CA certificates that outbound TLS clients trust besides their built-in roots, for self-hosted servers with a private CA (Holtmail's `tls_ca_file`). Adding roots never turns verification off: a server still needs a certificate for its name that chains to a built-in root or to one of these.

- `TlsRoots::from_pem(bytes)` reads every `CERTIFICATE` section of a PEM file in order and skips keys and other sections; `InvalidRoots` when a section does not decode or there is no certificate.
- `TlsRoots::from_der(certs)` keeps DER certificates as given; each client checks them when it is built and refuses an unusable one.
- `iter()` gives the DER bytes in order; `is_empty()`, `len()`. The default holds none, so clients keep their built-in roots only. Cloning is cheap; `Debug` shows only the count.

Clients that take them: `ImapSource`, `JmapSource`, `GraphSource`, `DavSource`, `JmapCollections`, `GraphCollections` and `HttpTokenEndpoint` (`with_roots`), `HttpFetcher` (`Options::roots`) and `S3BlobStore` (`S3Config::roots`).

### Testing

With the `testing` feature, `tls::testing::TestCa::new(&["localhost"])` makes a throwaway CA and a server certificate for those names, valid for 30 days. `roots()` is the CA as `TlsRoots`, `ca_pem()` its PEM, `acceptor()` a `tokio_rustls::TlsAcceptor` with the server certificate, and `serve_https(status, body)` an HTTPS server on a loopback port that answers every request with that status and JSON body. Client crates use it to prove that the CA is trusted only through their roots option.
