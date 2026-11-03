//! Holtmail's [`SearchIndex`] on [Tantivy](https://docs.rs/tantivy).

use std::ops::Bound;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use holtmail_core::{ArchiveId, MessageId};
use holtmail_index::*;
use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{
    AllQuery, BooleanQuery, FuzzyTermQuery, Occur, PhraseQuery, Query, RangeQuery, TermQuery,
};
use tantivy::schema::{
    Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
    Value,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, Language, LowerCaser, RawTokenizer, SimpleTokenizer, Stemmer, TextAnalyzer,
};
use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, Order, ReloadPolicy, TantivyDocument, Term,
};

const TOKENIZER: &str = "holtmail";
const STEMMER: &str = "holtmail-stem";
const MIN_PREFIX: usize = 3;
const MIN_STEM: usize = 4;
const WRITER_HEAP: usize = 64 * 1024 * 1024;
const MAX_CLAUSES: usize = 64;

#[derive(Clone, Copy)]
struct Fields {
    message_id: Field,
    archive_id: Field,
    subject: Field,
    from: Field,
    recipients: Field,
    body: Field,
    filenames: Field,
    attachment_text: Field,
    all: Field,
    folders: Field,
    flags: Field,
    date: Field,
    has_attachments: Field,
}

struct Inner {
    index: Index,
    fields: Fields,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
}

/// A Tantivy index in a directory, or in memory for tests.
///
/// Words are split on everything that is not a letter or digit, lowercased and folded to ASCII, so
/// `Zoë` finds `zoe` and `ada@example.com` is `ada example com`.
#[derive(Clone)]
pub struct TantivyIndex {
    inner: Arc<Inner>,
}

impl TantivyIndex {
    /// Opens the index in `dir`, creating it when the directory is empty.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, IndexError> {
        std::fs::create_dir_all(dir.as_ref()).map_err(backend)?;
        let directory = MmapDirectory::open(dir.as_ref()).map_err(backend)?;
        let (schema, fields) = schema();
        Self::with_index(
            Index::open_or_create(directory, schema).map_err(backend)?,
            fields,
        )
    }

    /// An empty index in memory.
    pub fn in_memory() -> Result<Self, IndexError> {
        let (schema, fields) = schema();
        Self::with_index(Index::create_in_ram(schema), fields)
    }

    fn with_index(index: Index, fields: Fields) -> Result<Self, IndexError> {
        index.tokenizers().register(
            TOKENIZER,
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(LowerCaser)
                .filter(AsciiFoldingFilter)
                .build(),
        );
        index.tokenizers().register(
            STEMMER,
            TextAnalyzer::builder(RawTokenizer::default())
                .filter(Stemmer::new(Language::English))
                .filter(Stemmer::new(Language::Russian))
                .build(),
        );
        let writer = index.writer(WRITER_HEAP).map_err(backend)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(backend)?;
        Ok(Self {
            inner: Arc::new(Inner {
                index,
                fields,
                writer: Mutex::new(writer),
                reader,
            }),
        })
    }
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let text = TextOptions::default().set_indexing_options(
        TextFieldIndexing::default()
            .set_tokenizer(TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions),
    );
    let fields = Fields {
        message_id: builder.add_text_field("message_id", STRING | STORED),
        archive_id: builder.add_text_field("archive_id", STRING),
        subject: builder.add_text_field("subject", text.clone()),
        from: builder.add_text_field("from", text.clone()),
        recipients: builder.add_text_field("recipients", text.clone()),
        body: builder.add_text_field("body", text.clone()),
        filenames: builder.add_text_field("filenames", text.clone()),
        attachment_text: builder.add_text_field("attachment_text", text.clone()),
        all: builder.add_text_field("all", text),
        folders: builder.add_text_field("folders", STRING),
        flags: builder.add_text_field("flags", STRING),
        date: builder.add_i64_field("date", INDEXED | tantivy::schema::FAST),
        has_attachments: builder.add_bool_field("has_attachments", INDEXED),
    };
    (builder.build(), fields)
}

fn document(fields: &Fields, doc: &IndexDocument) -> TantivyDocument {
    let mut out = TantivyDocument::new();
    out.add_text(fields.message_id, doc.message_id.to_string());
    out.add_text(fields.archive_id, doc.archive_id.to_string());
    out.add_text(fields.subject, &doc.subject);
    out.add_text(fields.all, &doc.subject);
    for address in &doc.from {
        let text = address_text(address);
        out.add_text(fields.from, &text);
        out.add_text(fields.all, &text);
    }
    for address in &doc.recipients {
        let text = address_text(address);
        out.add_text(fields.recipients, &text);
        out.add_text(fields.all, &text);
    }
    out.add_text(fields.body, &doc.body);
    out.add_text(fields.all, &doc.body);
    for name in &doc.attachment_names {
        out.add_text(fields.filenames, name);
        out.add_text(fields.all, name);
    }
    out.add_text(fields.attachment_text, &doc.attachment_text);
    out.add_text(fields.all, &doc.attachment_text);
    for folder in &doc.folders {
        out.add_text(fields.folders, folder.to_lowercase());
    }
    for flag in &doc.flags {
        out.add_text(fields.flags, flag);
    }
    out.add_i64(fields.date, doc.date.as_millis());
    out.add_bool(fields.has_attachments, doc.has_attachments);
    out
}

fn address_text(address: &holtmail_core::Address) -> String {
    match &address.name {
        Some(name) => format!("{name} {}", address.email),
        None => address.email.clone(),
    }
}

impl Inner {
    fn analyze(&self, analyzer: &str, text: &str) -> Vec<String> {
        let mut analyzer = self
            .index
            .tokenizers()
            .get(analyzer)
            .expect("analyzer is registered");
        let mut stream = analyzer.token_stream(text);
        let mut words = Vec::new();
        while stream.advance() {
            words.push(stream.token().text.clone());
        }
        words
    }

    fn word_query(&self, field: Field, word: &str, stem: bool) -> Box<dyn Query> {
        let term = |text: &str| Term::from_field_text(field, text);
        if word.chars().count() < MIN_PREFIX {
            return Box::new(TermQuery::new(term(word), IndexRecordOption::WithFreqs));
        }
        let starts_with = |text: &str| -> Box<dyn Query> {
            Box::new(FuzzyTermQuery::new_prefix(term(text), 0, true))
        };
        let root = stem
            .then(|| self.analyze(STEMMER, word).pop())
            .flatten()
            .filter(|root| root != word && root.chars().count() >= MIN_STEM);
        match root {
            Some(root) => Box::new(BooleanQuery::new(vec![
                (Occur::Should, starts_with(word)),
                (Occur::Should, starts_with(&root)),
            ])),
            None => starts_with(word),
        }
    }

    fn words_query(&self, field: Field, text: &str, as_phrase: bool, stem: bool) -> Box<dyn Query> {
        let words = self.analyze(TOKENIZER, text);
        match words.len() {
            0 => Box::new(AllQuery),
            1 => self.word_query(field, &words[0], stem),
            _ if as_phrase => Box::new(PhraseQuery::new(
                words
                    .iter()
                    .map(|w| Term::from_field_text(field, w))
                    .collect(),
            )),
            _ => Box::new(BooleanQuery::new(
                words
                    .iter()
                    .map(|w| (Occur::Must, self.word_query(field, w, stem)))
                    .collect(),
            )),
        }
    }

    fn clause(&self, clause: &Clause) -> (Occur, Box<dyn Query>) {
        let f = &self.fields;
        let exact = |field: Field, value: &str| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, value),
                IndexRecordOption::Basic,
            ))
        };
        let query: Box<dyn Query> = match clause {
            Clause::Text(text) => self.words_query(f.all, text, false, true),
            Clause::Phrase(text) => self.words_query(f.all, text, true, false),
            Clause::From(text) => self.words_query(f.from, text, true, false),
            Clause::To(text) => self.words_query(f.recipients, text, true, false),
            Clause::Subject(text) => self.words_query(f.subject, text, false, true),
            Clause::Body(text) => self.words_query(f.body, text, false, true),
            Clause::Filename(text) => self.words_query(f.filenames, text, true, false),
            Clause::HasAttachment => Box::new(TermQuery::new(
                Term::from_field_bool(f.has_attachments, true),
                IndexRecordOption::Basic,
            )),
            Clause::In(folder) => exact(f.folders, &folder.to_lowercase()),
            Clause::Flag(flag) => exact(f.flags, flag),
            Clause::Before(at) => Box::new(RangeQuery::new(
                Bound::Unbounded,
                Bound::Excluded(Term::from_field_i64(f.date, at.as_millis())),
            )),
            Clause::After(at) => Box::new(RangeQuery::new(
                Bound::Included(Term::from_field_i64(f.date, at.as_millis())),
                Bound::Unbounded,
            )),
            Clause::Not(inner) => {
                let (occur, query) = self.clause(inner);
                let negated = match occur {
                    Occur::MustNot => Occur::Must,
                    _ => Occur::MustNot,
                };
                return (negated, query);
            }
        };
        (Occur::Must, query)
    }

    fn query(&self, archive: ArchiveId, query: &SearchQuery) -> Result<BooleanQuery, IndexError> {
        if query.clauses.len() > MAX_CLAUSES {
            return Err(IndexError::Invalid(format!(
                "at most {MAX_CLAUSES} conditions in one search"
            )));
        }
        let mut parts: Vec<(Occur, Box<dyn Query>)> = vec![(
            Occur::Must,
            Box::new(TermQuery::new(
                Term::from_field_text(self.fields.archive_id, &archive.to_string()),
                IndexRecordOption::Basic,
            )),
        )];
        parts.extend(query.clauses.iter().map(|c| self.clause(c)));
        Ok(BooleanQuery::new(parts))
    }

    fn search(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        sort: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<SearchResults, IndexError> {
        let query = self.query(archive, query)?;
        let searcher = self.reader.searcher();
        let limit = limit.clamp(1, 1000) as usize;
        let top = TopDocs::with_limit(limit).and_offset(offset as usize);
        let (addresses, total): (Vec<DocAddress>, usize) = match sort {
            SortOrder::Newest => {
                let (docs, total) = searcher
                    .search(
                        &query,
                        &(top.order_by_fast_field::<i64>("date", Order::Desc), Count),
                    )
                    .map_err(backend)?;
                (docs.into_iter().map(|(_, a)| a).collect(), total)
            }
            SortOrder::Relevance => {
                let (docs, total) = searcher
                    .search(&query, &(top.order_by_score(), Count))
                    .map_err(backend)?;
                (docs.into_iter().map(|(_, a)| a).collect(), total)
            }
        };
        let mut hits = Vec::with_capacity(addresses.len());
        for address in addresses {
            let doc: TantivyDocument = searcher.doc(address).map_err(backend)?;
            let id = doc
                .get_first(self.fields.message_id)
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<MessageId>().ok())
                .ok_or_else(|| IndexError::Invalid("document without a message id".into()))?;
            hits.push(id);
        }
        Ok(SearchResults {
            hits,
            total: total as u64,
        })
    }
}

#[async_trait]
impl SearchIndex for TantivyIndex {
    async fn put(&self, documents: Vec<IndexDocument>) -> Result<(), IndexError> {
        let inner = self.inner.clone();
        blocking(move || {
            let writer = inner.writer.lock().map_err(|_| poisoned())?;
            for doc in &documents {
                writer.delete_term(Term::from_field_text(
                    inner.fields.message_id,
                    &doc.message_id.to_string(),
                ));
                writer
                    .add_document(document(&inner.fields, doc))
                    .map_err(backend)?;
            }
            Ok(())
        })
        .await
    }

    async fn remove(&self, ids: &[MessageId]) -> Result<(), IndexError> {
        let inner = self.inner.clone();
        let ids = ids.to_vec();
        blocking(move || {
            let writer = inner.writer.lock().map_err(|_| poisoned())?;
            for id in ids {
                writer.delete_term(Term::from_field_text(
                    inner.fields.message_id,
                    &id.to_string(),
                ));
            }
            Ok(())
        })
        .await
    }

    async fn commit(&self) -> Result<(), IndexError> {
        let inner = self.inner.clone();
        blocking(move || {
            inner
                .writer
                .lock()
                .map_err(|_| poisoned())?
                .commit()
                .map_err(backend)?;
            inner.reader.reload().map_err(backend)
        })
        .await
    }

    async fn search(
        &self,
        archive: ArchiveId,
        query: &SearchQuery,
        sort: SortOrder,
        offset: u32,
        limit: u32,
    ) -> Result<SearchResults, IndexError> {
        let inner = self.inner.clone();
        let query = query.clone();
        blocking(move || inner.search(archive, &query, sort, offset, limit)).await
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, IndexError> + Send + 'static,
) -> Result<T, IndexError> {
    tokio::task::spawn_blocking(work).await.map_err(backend)?
}

fn backend(error: impl std::error::Error + Send + Sync + 'static) -> IndexError {
    IndexError::Backend(Box::new(error))
}

fn poisoned() -> IndexError {
    IndexError::Invalid("index writer is unusable after a panic".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_search_index_contract() {
        holtmail_index::testing::conformance(Arc::new(TantivyIndex::in_memory().unwrap())).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keeps_the_contract_on_disk_and_reopens() {
        let dir = tempfile::tempdir().unwrap();
        holtmail_index::testing::conformance(Arc::new(TantivyIndex::open(dir.path()).unwrap()))
            .await;
        let reopened = TantivyIndex::open(dir.path()).unwrap();
        let results = reopened
            .search(
                ArchiveId::new(),
                &SearchQuery::default(),
                SortOrder::Newest,
                0,
                10,
            )
            .await
            .unwrap();
        assert_eq!(results.total, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn finds_other_forms_of_russian_and_english_words() {
        let index = TantivyIndex::in_memory().unwrap();
        let archive = ArchiveId::new();
        let message = |subject: &str, body: &str| IndexDocument {
            message_id: MessageId::new(),
            archive_id: archive,
            thread_id: holtmail_core::ThreadId::new(),
            subject: subject.into(),
            from: Vec::new(),
            recipients: Vec::new(),
            body: body.into(),
            attachment_names: Vec::new(),
            attachment_text: String::new(),
            folders: Vec::new(),
            flags: Vec::new(),
            date: holtmail_core::Timestamp::from_secs(1),
            has_attachments: false,
        };
        let lease = message("Договор аренды", "Подписанный договор во вложении.");
        let repairs = message("Lighthouse", "The repairs were finished on Monday.");
        index
            .put(vec![lease.clone(), repairs.clone()])
            .await
            .unwrap();
        index.commit().await.unwrap();

        for (words, expected) in [
            ("договора", lease.message_id),
            ("договорам", lease.message_id),
            ("аренда", lease.message_id),
            ("Подпис", lease.message_id),
            ("repair", repairs.message_id),
            ("repairing", repairs.message_id),
        ] {
            let found = index
                .search(
                    archive,
                    &SearchQuery {
                        clauses: vec![Clause::Text(words.into())],
                    },
                    SortOrder::Newest,
                    0,
                    10,
                )
                .await
                .unwrap();
            assert_eq!(found.hits, [expected], "{words}");
        }
    }
}
