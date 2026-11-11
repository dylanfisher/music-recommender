//! Reading one dump file as a stream of records: the file is read once, front to back,
//! through SHA-256, gzip and an XML tokenizer, and never unpacked to disk (the releases XML
//! is about 80 GB). The hash covers every compressed byte, and the stream fails at its end
//! unless it matches the CHECKSUM's, so a dump counts as read only when the whole file is
//! the one Discogs published. Each dump's record type ([`Record`]) lives in its own module.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use flate2::bufread::MultiGzDecoder;
use quick_xml::XmlVersion;
use quick_xml::escape::resolve_predefined_entity;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use sha2::{Digest, Sha256};

/// The read size from the file, which may be on the NAS: large reads suit SMB.
const FILE_BUFFER: usize = 1 << 20;

/// The read size from the decompressor into the tokenizer.
const XML_BUFFER: usize = 256 << 10;

/// One dump's record.
pub trait Record: Sized {
    /// The document's root element, which holds the records.
    const ROOT: &'static str;
    /// Each record's element.
    const ELEMENT: &'static str;
    /// What the record is built in while its element is read.
    type Reading: Reading<Record = Self>;

    /// The record's Discogs id.
    fn id(&self) -> u64;
}

/// A record being built from the events of its element.
pub trait Reading: Default {
    /// The record it becomes.
    type Record;

    /// An element opens. `path` is its place under the record's element, its names joined
    /// with `/` (`tracklist/track/duration`), and empty for the record's own element.
    ///
    /// # Errors
    ///
    /// A value the record can't hold, in words.
    fn open(&mut self, path: &str, element: &BytesStart<'_>) -> Result<(), String>;

    /// The element at `path` closes; `text` is the text directly in it, references
    /// resolved. The record's own element closes last, with an empty `path`, where the
    /// record checks it has what it must.
    ///
    /// # Errors
    ///
    /// A value the record can't hold, in words.
    fn close(&mut self, path: &str, text: &str) -> Result<(), String>;

    /// The record read.
    fn finish(self) -> Self::Record;
}

/// Where a master's and a release's artists, genres and styles are, under the record.
pub(crate) const ARTIST_ID: &str = "artists/artist/id";
pub(crate) const GENRE: &str = "genres/genre";
pub(crate) const STYLE: &str = "styles/style";

/// What an artist id that isn't a number is called in the error.
pub(crate) const ARTIST_ID_NAME: &str = "artist id";

/// Fails when a record whose id is an element of its own closed without one.
///
/// # Errors
///
/// When `id` is 0, which no Discogs record has.
pub(crate) fn require_id(id: u64) -> Result<(), String> {
    if id == 0 {
        Err("no id".to_string())
    } else {
        Ok(())
    }
}

/// The value of the attribute `name` of `element`, references resolved.
///
/// # Errors
///
/// When the element's attributes aren't well formed.
pub(crate) fn attribute(element: &BytesStart<'_>, name: &str) -> Result<Option<String>, String> {
    element
        .try_get_attribute(name)
        .map_err(|e| e.to_string())?
        .map(|value| {
            value
                .normalized_value(XmlVersion::Implicit1_0)
                .map(std::borrow::Cow::into_owned)
                .map_err(|e| e.to_string())
        })
        .transpose()
}

/// A whole number that must be there, such as a record's id.
///
/// # Errors
///
/// When `text` is absent or isn't a whole number; `what` names it.
pub(crate) fn number(text: Option<&str>, what: &str) -> Result<u64, String> {
    let text = text.ok_or_else(|| format!("no {what}"))?;
    text.trim()
        .parse()
        .map_err(|_| format!("{what} `{text}` isn't a whole number"))
}

/// A finished SHA-256 as lowercase hex, as CHECKSUM files write it.
pub(crate) fn hex(hash: Sha256) -> String {
    hash.finalize()
        .iter()
        .fold(String::new(), |hex, byte| hex + &format!("{byte:02x}"))
}

/// The bytes read from the file pass through the hash on their way to the decompressor.
struct Hashing {
    file: File,
    hash: Sha256,
}

impl Read for Hashing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(buf)?;
        self.hash.update(&buf[..read]);
        Ok(read)
    }
}

type Tokenizer = quick_xml::Reader<BufReader<MultiGzDecoder<BufReader<Hashing>>>>;

/// A dump file being read, one record at a time.
pub struct Stream<R> {
    path: PathBuf,
    sha256: String,
    xml: Tokenizer,
    buf: Vec<u8>,
    /// The element path inside the record being read, and where each level starts in it.
    path_text: String,
    levels: Vec<usize>,
    text: String,
    records: u64,
    state: State,
    record: PhantomData<R>,
}

/// Where the stream is in the document.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Before the root element.
    Start,
    /// Inside the root, between records.
    Records,
    /// The root closed and the checksum matched.
    Done,
}

/// Why a dump couldn't be read. Each names the file.
#[derive(Debug)]
pub struct DumpError {
    path: PathBuf,
    problem: Problem,
}

#[derive(Debug)]
enum Problem {
    Io(io::Error),
    Truncated,
    Xml(String),
    Record { number: u64, message: String },
    Checksum { expected: String, found: String },
}

impl fmt::Display for DumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = self.path.display();
        match &self.problem {
            Problem::Io(error) => write!(f, "{path}: {error}"),
            Problem::Truncated => write!(f, "{path}: truncated: the file ends inside the dump"),
            Problem::Xml(message) => write!(f, "{path}: not the XML of a dump: {message}"),
            Problem::Record { number, message } => write!(f, "{path}: record {number}: {message}"),
            Problem::Checksum { expected, found } => write!(
                f,
                "{path}: SHA-256 is {found}, but the CHECKSUM file lists {expected}"
            ),
        }
    }
}

impl std::error::Error for DumpError {}

impl DumpError {
    /// Whether the file was read to its end and its SHA-256 isn't the CHECKSUM's: the
    /// records read from it came from another file than the one Discogs published.
    #[must_use]
    pub fn is_checksum(&self) -> bool {
        matches!(self.problem, Problem::Checksum { .. })
    }
}

impl<R: Record> Stream<R> {
    /// Opens the dump at `path`, whose SHA-256 should be `sha256` (lowercase hex).
    ///
    /// # Errors
    ///
    /// When the file can't be opened.
    pub fn open(path: &Path, sha256: &str) -> Result<Self, DumpError> {
        let file = File::open(path).map_err(|error| DumpError {
            path: path.to_path_buf(),
            problem: Problem::Io(error),
        })?;
        let hashing = Hashing {
            file,
            hash: Sha256::new(),
        };
        let gzip = MultiGzDecoder::new(BufReader::with_capacity(FILE_BUFFER, hashing));
        Ok(Self {
            path: path.to_path_buf(),
            sha256: sha256.to_string(),
            xml: quick_xml::Reader::from_reader(BufReader::with_capacity(XML_BUFFER, gzip)),
            buf: Vec::new(),
            path_text: String::new(),
            levels: Vec::new(),
            text: String::new(),
            records: 0,
            state: State::Start,
            record: PhantomData,
        })
    }

    /// The records read or skipped so far.
    #[must_use]
    pub fn records(&self) -> u64 {
        self.records
    }

    /// The next record, or `None` after the last one, once the whole file has been read
    /// and its SHA-256 matched.
    ///
    /// # Errors
    ///
    /// When the file can't be read, is truncated, isn't a dump's XML, has a record the
    /// parser refuses, or its SHA-256 isn't the CHECKSUM's.
    pub fn read(&mut self) -> Result<Option<R>, DumpError> {
        let Some((element, empty)) = self.seek_record()? else {
            return Ok(None);
        };
        let number = self.records + 1;
        let mut record = R::Reading::default();
        let refused = |message| Problem::Record { number, message };
        record
            .open("", &element)
            .map_err(refused)
            .and_then(|()| {
                if empty {
                    return record.close("", "").map_err(refused);
                }
                self.walk(&mut record).map_err(|e| e.into_record(number))
            })
            .map_err(|problem| self.fail(problem))?;
        self.records = number;
        Ok(Some(record.finish()))
    }

    /// Skips `count` records, reading the last of them in full, and returns its id: `None`
    /// if the dump has fewer, or `count` is 0. The others are passed over by the tokenizer
    /// without being built.
    ///
    /// # Errors
    ///
    /// As `next`.
    pub fn skip(&mut self, count: u64) -> Result<Option<u64>, DumpError> {
        for _ in 1..count {
            match self.seek_record()? {
                None => return Ok(None),
                Some((_, true)) => {}
                Some((_, false)) => {
                    self.buf.clear();
                    let skipped = self.xml.read_to_end_into(QName(R::ELEMENT), &mut self.buf);
                    skipped.map_err(|e| self.fail(xml_problem(&e)))?;
                }
            }
            self.records += 1;
        }
        if count == 0 {
            return Ok(None);
        }
        Ok(self.read()?.map(|record| record.id()))
    }

    /// Reads up to the next record's start and returns it, and whether the record is an
    /// empty element, or reads to the end of the document, checks the hash and returns
    /// `None`.
    fn seek_record(&mut self) -> Result<Option<(BytesStart<'static>, bool)>, DumpError> {
        loop {
            if self.state == State::Done {
                return Ok(None);
            }
            self.buf.clear();
            let position = self.xml.buffer_position();
            let event = match self.xml.read_event_into(&mut self.buf) {
                Ok(event) => event,
                Err(error) => return Err(self.fail(xml_problem(&error))),
            };
            let problem = match (self.state, event) {
                (State::Start, Event::Start(e)) if e.name().as_ref() == R::ROOT => {
                    self.state = State::Records;
                    continue;
                }
                (State::Records, Event::Start(e)) if e.name().as_ref() == R::ELEMENT => {
                    return Ok(Some((e.into_owned(), false)));
                }
                (State::Records, Event::Empty(e)) if e.name().as_ref() == R::ELEMENT => {
                    return Ok(Some((e.into_owned(), true)));
                }
                (State::Records, Event::End(_)) => {
                    self.finish()?;
                    self.state = State::Done;
                    continue;
                }
                (_, Event::Eof) => Problem::Truncated,
                (_, Event::Start(e) | Event::Empty(e)) => Problem::Xml(format!(
                    "unexpected element <{}> at byte {position}",
                    e.name().as_ref()
                )),
                _ => continue,
            };
            return Err(self.fail(problem));
        }
    }

    /// Reads the record's events up to its end, telling `record` of each element.
    fn walk(&mut self, record: &mut R::Reading) -> Result<(), Problem> {
        self.path_text.clear();
        self.levels.clear();
        self.text.clear();
        loop {
            self.buf.clear();
            let event = self
                .xml
                .read_event_into(&mut self.buf)
                .map_err(|e| xml_problem(&e))?;
            match event {
                Event::Start(element) => {
                    enter(
                        &mut self.path_text,
                        &mut self.levels,
                        element.name().as_ref(),
                    );
                    self.text.clear();
                    record
                        .open(&self.path_text, &element)
                        .map_err(Problem::refused)?;
                }
                Event::Empty(element) => {
                    enter(
                        &mut self.path_text,
                        &mut self.levels,
                        element.name().as_ref(),
                    );
                    record
                        .open(&self.path_text, &element)
                        .map_err(Problem::refused)?;
                    record
                        .close(&self.path_text, "")
                        .map_err(Problem::refused)?;
                    leave(&mut self.path_text, &mut self.levels);
                    self.text.clear();
                }
                Event::Text(text) => self.text.push_str(&text),
                Event::CData(text) => self.text.push_str(&text),
                Event::GeneralRef(reference) => {
                    if let Some(c) = reference.resolve_char_ref().map_err(|e| xml_problem(&e))? {
                        self.text.push(c);
                    } else {
                        let resolved = resolve_predefined_entity(&reference).ok_or_else(|| {
                            Problem::Xml(format!("unknown entity &{};", &*reference))
                        })?;
                        self.text.push_str(resolved);
                    }
                }
                Event::End(_) if self.levels.is_empty() => {
                    return record.close("", &self.text).map_err(Problem::refused);
                }
                Event::End(_) => {
                    record
                        .close(&self.path_text, &self.text)
                        .map_err(Problem::refused)?;
                    leave(&mut self.path_text, &mut self.levels);
                    self.text.clear();
                }
                Event::Eof => return Err(Problem::Truncated),
                _ => {}
            }
        }
    }

    /// After the root closes: reads to the end of the file, so the hash covers every byte,
    /// and compares it with the CHECKSUM's.
    fn finish(&mut self) -> Result<(), DumpError> {
        loop {
            self.buf.clear();
            match self.xml.read_event_into(&mut self.buf) {
                Ok(Event::Eof) => break,
                Ok(Event::Start(_) | Event::Empty(_) | Event::Text(_) | Event::End(_)) => {
                    // Whitespace after the root is a Text event; anything else is not a dump.
                    let after = String::from_utf8_lossy(&self.buf).trim().is_empty();
                    if !after {
                        return Err(self.fail(Problem::Xml("content after the root".into())));
                    }
                }
                Ok(_) => {}
                Err(error) => return Err(self.fail(xml_problem(&error))),
            }
        }
        let gzip = self.xml.get_mut().get_mut();
        let drained =
            io::copy(gzip, &mut io::sink()).and_then(|_| io::copy(gzip.get_mut(), &mut io::sink()));
        if let Err(error) = drained {
            return Err(self.fail(Problem::Io(error)));
        }
        let found = self
            .xml
            .get_ref()
            .get_ref()
            .get_ref()
            .get_ref()
            .hash
            .clone();
        let found = hex(found);
        if found == self.sha256 {
            Ok(())
        } else {
            Err(self.fail(Problem::Checksum {
                expected: self.sha256.clone(),
                found,
            }))
        }
    }

    fn fail(&self, problem: Problem) -> DumpError {
        DumpError {
            path: self.path.clone(),
            problem,
        }
    }
}

impl Problem {
    fn refused(message: String) -> Self {
        Self::Record { number: 0, message }
    }

    /// The problem as one of record `number`'s, where it came from inside a record.
    fn into_record(self, number: u64) -> Self {
        match self {
            Self::Record { message, .. } => Self::Record { number, message },
            other => other,
        }
    }
}

/// A tokenizer error; a read that ended early is a truncated file.
fn xml_problem(error: &quick_xml::Error) -> Problem {
    match error {
        quick_xml::Error::Io(io) if io.kind() == io::ErrorKind::UnexpectedEof => Problem::Truncated,
        quick_xml::Error::Io(io) => Problem::Io(io::Error::new(io.kind(), io.to_string())),
        other => Problem::Xml(other.to_string()),
    }
}

/// Adds an element to the path.
fn enter(path: &mut String, levels: &mut Vec<usize>, name: &str) {
    levels.push(path.len());
    if !path.is_empty() {
        path.push('/');
    }
    path.push_str(name);
}

/// Takes the last element off the path.
fn leave(path: &mut String, levels: &mut Vec<usize>) {
    if let Some(len) = levels.pop() {
        path.truncate(len);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::Write;
    use std::path::PathBuf;

    use flate2::Compression;
    use flate2::write::GzEncoder;
    use quick_xml::events::BytesStart;
    use sha2::{Digest, Sha256};

    use super::{Reading, Record, Stream, attribute};

    /// A record of the test's own: `<item n="..."><name>...</name></item>`.
    #[derive(Debug, Default, PartialEq)]
    struct Item {
        id: u64,
        name: String,
    }

    impl Record for Item {
        const ROOT: &'static str = "items";
        const ELEMENT: &'static str = "item";
        type Reading = Self;

        fn id(&self) -> u64 {
            self.id
        }
    }

    impl Reading for Item {
        type Record = Self;

        fn finish(self) -> Self {
            self
        }

        fn open(&mut self, path: &str, element: &BytesStart<'_>) -> Result<(), String> {
            if path.is_empty() {
                self.id = attribute(element, "n")?
                    .ok_or("no n")?
                    .parse()
                    .map_err(|_| "n isn't a number")?;
            }
            Ok(())
        }

        fn close(&mut self, path: &str, text: &str) -> Result<(), String> {
            if path == "name" {
                self.name = text.to_string();
            }
            Ok(())
        }
    }

    const XML: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<items>\
        <item n=\"1\"><name>Simon &amp; Garfunkel</name></item>\n\
        <item n=\"2\"><name>Caf&#233; <![CDATA[<b>]]></name><extra><name>x</name></extra></item>\n\
        <item n=\"5\"><name/></item>\n</items>\n";

    /// Writes `xml` gzipped to a file named for the test and returns its path and SHA-256.
    pub(crate) fn gz(test: &str, xml: &str) -> (PathBuf, String) {
        let path = std::env::temp_dir().join(format!("dump-{test}-{}.xml.gz", std::process::id()));
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        let bytes = encoder.finish().unwrap();
        std::fs::write(&path, &bytes).unwrap();
        (path, hex(&bytes))
    }

    fn hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |text, b| text + &format!("{b:02x}"))
    }

    fn all(stream: &mut Stream<Item>) -> Vec<Item> {
        let mut items = Vec::new();
        while let Some(item) = stream.read().unwrap() {
            items.push(item);
        }
        items
    }

    #[test]
    fn reads_each_record_with_its_text_unescaped() {
        let (path, sha) = gz("read", XML);
        let mut stream = Stream::<Item>::open(&path, &sha).unwrap();
        let items = all(&mut stream);
        assert_eq!(
            items,
            [
                Item {
                    id: 1,
                    name: "Simon & Garfunkel".to_string()
                },
                Item {
                    id: 2,
                    name: "Café <b>".to_string()
                },
                Item {
                    id: 5,
                    name: String::new()
                },
            ]
        );
        assert_eq!(stream.records(), 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn skips_records_and_says_the_id_of_the_last_one_skipped() {
        let (path, sha) = gz("skip", XML);
        let mut stream = Stream::<Item>::open(&path, &sha).unwrap();
        assert_eq!(stream.skip(2).unwrap(), Some(2));
        assert_eq!(stream.records(), 2);
        assert_eq!(all(&mut stream)[0].id, 5);
        let mut stream = Stream::<Item>::open(&path, &sha).unwrap();
        assert_eq!(stream.skip(4).unwrap(), None);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_wrong_checksum_fails_after_the_last_record() {
        let (path, sha) = gz("checksum", XML);
        let wrong = "0".repeat(64);
        let mut stream = Stream::<Item>::open(&path, &wrong).unwrap();
        for _ in 0..3 {
            stream.read().unwrap().unwrap();
        }
        let error = stream.read().unwrap_err().to_string();
        assert!(error.contains(&wrong) && error.contains(&sha), "{error}");
        assert!(error.contains(&path.display().to_string()), "{error}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_truncated_file_fails() {
        let big = (1..=2000).fold(String::new(), |text, n| {
            text + &format!("<item n=\"{n}\"><name>name {n}</name></item>\n")
        });
        let (path, _) = gz("truncated", &format!("<items>{big}</items>"));
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() / 2]).unwrap();
        let sha = hex(&bytes[..bytes.len() / 2]);
        let mut stream = Stream::<Item>::open(&path, &sha).unwrap();
        let error = loop {
            match stream.read() {
                Ok(Some(_)) => {}
                Ok(None) => panic!("read to the end of a truncated file"),
                Err(error) => break error.to_string(),
            }
        };
        assert!(stream.records() > 0, "nothing read before the cut");
        assert!(error.contains("truncated"), "{error}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_record_that_fails_names_its_position() {
        let (path, sha) = gz("bad", "<items><item n=\"1\"/><item n=\"x\"/></items>");
        let mut stream = Stream::<Item>::open(&path, &sha).unwrap();
        stream.read().unwrap().unwrap();
        let error = stream.read().unwrap_err().to_string();
        assert!(
            error.contains("record 2") && error.contains("n isn't a number"),
            "{error}"
        );
        let _ = std::fs::remove_file(path);
    }
}
