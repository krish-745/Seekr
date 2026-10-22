//! The full-text index: one Tantivy document per passage, ranked with BM25, with highlighted
//! snippets. The indexer writes it; the app opens it read-only and sees each commit within a
//! moment.

use std::path::Path;

use tantivy::{
    DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, Score, TantivyDocument, Term,
    collector::TopDocs,
    directory::MmapDirectory,
    doc,
    query::{BooleanQuery, Occur, QueryParser, TermQuery},
    schema::{
        Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions,
        Value,
    },
    snippet::SnippetGenerator,
};

use crate::chunk::Chunk;

/// Indexing memory; the writer flushes a segment when it fills.
const WRITER_HEAP: usize = 64 * 1024 * 1024;
const SNIPPET_CHARS: usize = 180;

pub struct Store {
    index: Index,
    path: Field,
    body: Field,
    line: Field,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: String,
    pub line: u32,
    pub score: Score,
    /// The best passage fragment, and the byte ranges in it that matched.
    pub snippet: String,
    pub highlights: Vec<(usize, usize)>,
}

impl Store {
    /// Opens the index in `dir`, creating it if needed.
    pub fn open(dir: &Path) -> tantivy::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let mut builder = Schema::builder();
        let path = builder.add_text_field("path", STRING | STORED);
        // English stemming, so "invoices" finds "invoice".
        let indexing = TextFieldIndexing::default()
            .set_tokenizer("en_stem")
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let body = builder.add_text_field(
            "body",
            TextOptions::default()
                .set_indexing_options(indexing)
                .set_stored(),
        );
        let line = builder.add_u64_field("line", STORED | INDEXED);
        let schema = builder.build();
        let index = Index::open_or_create(MmapDirectory::open(dir)?, schema)?;
        Ok(Self {
            index,
            path,
            body,
            line,
        })
    }

    /// The only writer: the index's lock file keeps a second one out.
    pub fn writer(&self) -> tantivy::Result<IndexWriter> {
        self.index.writer(WRITER_HEAP)
    }

    /// Replaces every passage of `path`; visible after the writer commits.
    pub fn replace(
        &self,
        writer: &IndexWriter,
        path: &str,
        chunks: &[Chunk],
    ) -> tantivy::Result<()> {
        self.remove(writer, path);
        for chunk in chunks {
            writer.add_document(doc!(
                self.path => path,
                self.body => chunk.text.as_str(),
                self.line => u64::from(chunk.line),
            ))?;
        }
        Ok(())
    }

    pub fn remove(&self, writer: &IndexWriter, path: &str) {
        writer.delete_term(Term::from_field_text(self.path, path));
    }

    /// A reader that picks up the indexer's commits by itself.
    pub fn reader(&self) -> tantivy::Result<IndexReader> {
        self.index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
    }

    /// The text of the passage of `path` that starts at `line`.
    pub fn passage(&self, reader: &IndexReader, path: &str, line: u32) -> Option<String> {
        let searcher = reader.searcher();
        let term = |t: Term| -> (Occur, Box<dyn tantivy::query::Query>) {
            (
                Occur::Must,
                Box::new(TermQuery::new(t, IndexRecordOption::Basic)),
            )
        };
        let query = BooleanQuery::new(vec![
            term(Term::from_field_text(self.path, path)),
            term(Term::from_field_u64(self.line, u64::from(line))),
        ]);
        let top = searcher
            .search(&query, &TopDocs::with_limit(1).order_by_score())
            .ok()?;
        let doc: TantivyDocument = searcher.doc(top.first()?.1).ok()?;
        doc.get_first(self.body)?.as_str().map(String::from)
    }

    /// The best passage of each of the `limit` best files for `query`. Every word must appear
    /// (stemmed); quotes, `-word` and other query syntax work, and malformed syntax is read as
    /// plain words rather than failing.
    pub fn search(
        &self,
        reader: &IndexReader,
        query: &str,
        limit: usize,
    ) -> tantivy::Result<Vec<Hit>> {
        let searcher = reader.searcher();
        let mut parser = QueryParser::for_index(&self.index, vec![self.body]);
        parser.set_conjunction_by_default();
        let (query, _errors) = parser.parse_query_lenient(query);
        // Several passages of one file can rank high; fetch extra and keep each file's best.
        let top: Vec<(Score, DocAddress)> =
            searcher.search(&query, &TopDocs::with_limit(limit * 4).order_by_score())?;
        let mut snippets = SnippetGenerator::create(&searcher, &*query, self.body)?;
        snippets.set_max_num_chars(SNIPPET_CHARS);
        let mut hits: Vec<Hit> = Vec::new();
        for (score, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let Some(path) = doc.get_first(self.path).and_then(|v| v.as_str()) else {
                continue;
            };
            if hits.iter().any(|h| h.path == path) {
                continue;
            }
            let line = doc
                .get_first(self.line)
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            let snippet = snippets.snippet_from_doc(&doc);
            // The line of the first highlighted word, not where the passage starts.
            let body = doc
                .get_first(self.body)
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let first = snippet.highlighted().first().map_or(0, |r| r.start);
            let below = body
                .find(snippet.fragment())
                .and_then(|at| body.get(..at + first))
                .map_or(0, |before| before.matches('\n').count());
            hits.push(Hit {
                path: path.to_string(),
                line: (line as usize + below) as u32,
                score,
                snippet: snippet.fragment().to_string(),
                highlights: snippet
                    .highlighted()
                    .iter()
                    .map(|r| (r.start, r.end))
                    .collect(),
            });
            if hits.len() == limit {
                break;
            }
        }
        Ok(hits)
    }

    /// Whole passages for a question, best first: any of its words may match (a question has
    /// words no passage shares), and no file gives more than two. Each comes as (path, the line
    /// it starts on, its text).
    pub fn passages(
        &self,
        reader: &IndexReader,
        question: &str,
        limit: usize,
    ) -> tantivy::Result<Vec<(String, u32, String)>> {
        let searcher = reader.searcher();
        let parser = QueryParser::for_index(&self.index, vec![self.body]);
        let (query, _errors) = parser.parse_query_lenient(question);
        let top: Vec<(Score, DocAddress)> =
            searcher.search(&query, &TopDocs::with_limit(limit * 3).order_by_score())?;
        let mut found: Vec<(String, u32, String)> = Vec::new();
        for (_, address) in top {
            let doc: TantivyDocument = searcher.doc(address)?;
            let field = |f| {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            let (Some(path), Some(text)) = (field(self.path), field(self.body)) else {
                continue;
            };
            if found.iter().filter(|p| p.0 == path).count() >= 2 {
                continue;
            }
            let line = doc
                .get_first(self.line)
                .and_then(|v| v.as_u64())
                .unwrap_or(1);
            found.push((path, line as u32, text));
            if found.len() == limit {
                break;
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("seekr-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn chunk(text: &str, line: u32) -> Chunk {
        Chunk {
            text: text.into(),
            line,
        }
    }

    #[test]
    fn finds_stemmed_words_and_keeps_one_hit_per_file() {
        let dir = temp("find");
        let store = Store::open(&dir).unwrap();
        let mut writer = store.writer().unwrap();
        store
            .replace(
                &writer,
                r"C:\a.md",
                &[
                    chunk("Quarterly invoices are due", 1),
                    chunk("more invoice talk", 9),
                ],
            )
            .unwrap();
        store
            .replace(&writer, r"C:\b.txt", &[chunk("nothing relevant here", 1)])
            .unwrap();
        store
            .replace(
                &writer,
                r"C:\c.txt",
                &[chunk("filler\nmore filler\nthe zanzibar quokka", 4)],
            )
            .unwrap();
        writer.commit().unwrap();
        let reader = store.reader().unwrap();
        reader.reload().unwrap();
        let hits = store.search(&reader, "invoice", 10).unwrap();
        assert_eq!(hits.len(), 1, "one hit per file");
        assert_eq!(hits[0].path, r"C:\a.md");
        let (s, e) = hits[0].highlights[0];
        assert!(hits[0].snippet[s..e].to_lowercase().starts_with("invoice"));
        assert!(
            store
                .search(&reader, "invoice zebra", 10)
                .unwrap()
                .is_empty(),
            "all words"
        );
        assert!(store.search(&reader, "unclosed \"quote", 10).is_ok());
        let hits = store.search(&reader, "quokka", 10).unwrap();
        assert_eq!(
            hits[0].line, 6,
            "the matching line, two below the passage start"
        );
        let found = store
            .passages(&reader, "what is a quokka invoice", 10)
            .unwrap();
        assert!(
            found.iter().any(|p| p.0 == r"C:\c.txt" && p.1 == 4),
            "any word will do"
        );
        assert!(found.iter().any(|p| p.0 == r"C:\a.md"));
        assert_eq!(found.iter().filter(|p| p.0 == r"C:\a.md").count(), 2);

        assert_eq!(
            store.passage(&reader, r"C:\a.md", 9).as_deref(),
            Some("more invoice talk")
        );
        assert_eq!(store.passage(&reader, r"C:\a.md", 2), None);
        store.remove(&writer, r"C:\a.md");
        writer.commit().unwrap();
        reader.reload().unwrap();
        assert!(store.search(&reader, "invoice", 10).unwrap().is_empty());
        drop(writer);
        assert!(store.writer().is_ok(), "the lock goes with the writer");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
