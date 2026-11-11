//! The generated set the budgets are measured on (ADR 0022), for the features tests that
//! include this file with `#[path]`: releases in the shape of the dumps' with about their
//! averages, and artists, labels and masters in proportion, written as a complete set of
//! the fixture set's date. Each dump is streamed through the system's `gzip` and hashed with
//! `shasum`, the benchmarks' own tools: features has no gzip or SHA-256 of its own.

#![allow(
    dead_code,
    reason = "each test crate that includes this file uses part of it"
)]

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// The set's date.
const DATE: &str = "20000101";

/// Writes the set of `releases` generated releases in `dir`, and returns the releases XML's
/// size in bytes.
pub(crate) fn write_set(dir: &Path, releases: u64) -> u64 {
    let mut checksum = String::new();
    let (line, _) = write_dump(dir, "artists", |out| {
        writeln!(out, "<artists>").unwrap();
        for id in 1..=releases * 53 / 100 {
            let aliases = if id % 15 == 0 {
                format!("<aliases><name id=\"{}\">Alias</name></aliases>", id + 1)
            } else {
                String::new()
            };
            writeln!(
                out,
                "<artist><id>{id}</id><name>Artist {id}</name><realname>Real Name {id}</realname>\
                 <profile>Made up.</profile><namevariations><name>A. {id}</name></namevariations>\
                 {aliases}</artist>"
            )
            .unwrap();
        }
        writeln!(out, "</artists>").unwrap();
    });
    checksum += &line;
    let (line, _) = write_dump(dir, "labels", |out| {
        writeln!(out, "<labels>").unwrap();
        for id in 1..=releases / 8 {
            writeln!(
                out,
                "<label><id>{id}</id><name>Label {id}</name><profile>Made up.</profile></label>"
            )
            .unwrap();
        }
        writeln!(out, "</labels>").unwrap();
    });
    checksum += &line;
    let (line, _) = write_dump(dir, "masters", |out| {
        writeln!(out, "<masters>").unwrap();
        for id in 1..=releases / 7 {
            writeln!(
                out,
                "<master id=\"{id}\"><main_release>{}</main_release><artists><artist><id>{}</id>\
                 <name>A</name></artist></artists><genres><genre>Electronic</genre></genres>\
                 <styles><style>Style {}</style></styles><year>1999</year><title>Master {id}</title>\
                 </master>",
                id * 7,
                id % (releases / 2) + 1,
                id % 700
            )
            .unwrap();
        }
        writeln!(out, "</masters>").unwrap();
    });
    checksum += &line;
    let (line, bytes) = write_dump(dir, "releases", |out| {
        writeln!(out, "<releases>").unwrap();
        for id in 1..=releases {
            out.write_all(release(id, releases).as_bytes()).unwrap();
        }
        writeln!(out, "</releases>").unwrap();
    });
    checksum += &line;
    fs::write(dir.join(format!("discogs_{DATE}_CHECKSUM.txt")), checksum).unwrap();
    bytes
}

/// Writes what `write` writes gzipped as the set's `dump` in `dir`, and returns its CHECKSUM
/// line and the size of the XML.
fn write_dump(dir: &Path, dump: &str, write: impl FnOnce(&mut dyn Write)) -> (String, u64) {
    let name = format!("discogs_{DATE}_{dump}.xml.gz");
    let mut gzip = Command::new("gzip")
        .args(["-c", "-6"])
        .stdin(Stdio::piped())
        .stdout(File::create(dir.join(&name)).unwrap())
        .spawn()
        .unwrap();
    let mut counted = Counted {
        inner: BufWriter::new(gzip.stdin.take().unwrap()),
        bytes: 0,
    };
    write(&mut counted);
    let bytes = counted.bytes;
    counted.inner.flush().unwrap();
    drop(counted);
    assert!(gzip.wait().unwrap().success());
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(dir.join(&name))
        .output()
        .unwrap();
    let hash = String::from_utf8(out.stdout).unwrap();
    (format!("{}  {name}\n", &hash[..64]), bytes)
}

/// A writer that counts the bytes written through it.
struct Counted<W> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for Counted<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.bytes += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Release `id` of a set of `releases`, in the shape of the dumps' with about their
/// averages: nine tracks, half with a duration, a few credits at the release and the track
/// level, one or two labels, formats with descriptions, a master for most, notes, images,
/// videos and companies the parser passes over. Its artist and credits are among a set's
/// first half of artists, and its label among the labels.
fn release(id: u64, releases: u64) -> String {
    let artists = releases / 2;
    let labels = releases / 8;
    let mut xml = format!(
        "<release id=\"{id}\" status=\"Accepted\"><images><image height=\"600\" \
         type=\"primary\" uri=\"\" uri150=\"\" width=\"600\"/><image height=\"600\" \
         type=\"secondary\" uri=\"\" uri150=\"\" width=\"600\"/></images><artists><artist>\
         <id>{}</id><name>Artist {}</name><anv></anv><join></join><role></role><tracks></tracks>\
         </artist></artists><title>Generated Release Number {id}</title><labels>\
         <label name=\"Label {}\" catno=\"CAT-{id}\" id=\"{}\"/></labels><extraartists>",
        id % artists + 1,
        id % artists + 1,
        id % labels + 1,
        id % labels + 1,
    );
    for n in 0..3 {
        let _ = write!(
            xml,
            "<artist><id>{}</id><name>Credit {n}</name><anv></anv><join></join>\
             <role>{} [Tracks A1 to B2], Producer</role><tracks></tracks></artist>",
            (id * 7 + n) % artists + 1,
            ["Written-By", "Mixed By", "Engineer"][usize::try_from(n).unwrap()]
        );
    }
    let _ = write!(
        xml,
        "</extraartists><formats><format name=\"Vinyl\" qty=\"1\" text=\"\"><descriptions>\
         <description>LP</description><description>Album</description></descriptions>\
         </format></formats><genres><genre>Electronic</genre></genres><styles>\
         <style>Style {}</style><style>Style {}</style></styles><country>UK</country>\
         <released>1999-03-00</released><notes>Recorded at a made-up studio. All tracks \
         made up for the benchmark; nothing here is real &amp; nothing is kept.</notes>\
         <data_quality>Needs Vote</data_quality><master_id is_main_release=\"{}\">{}</master_id>\
         <tracklist>",
        id % 700,
        id % 53,
        id.is_multiple_of(5),
        if id % 10 < 6 { id / 7 + 1 } else { 0 }
    );
    for track in 0..9 {
        let duration = if (id + track).is_multiple_of(2) {
            "4:05"
        } else {
            ""
        };
        let _ = write!(
            xml,
            "<track><position>{}{}</position><title>Track Title Number {track}</title>\
             <duration>{duration}</duration></track>",
            if track < 5 { 'A' } else { 'B' },
            track % 5 + 1
        );
    }
    let _ = writeln!(
        xml,
        "</tracklist><identifiers>{}<identifier type=\"Matrix / Runout\" description=\"A\" \
         value=\"CAT-{id}-A\"/></identifiers><videos><video duration=\"245\" embed=\"true\" \
         src=\"https://example.com/{id}\"><title>Track</title><description>Track</description>\
         </video></videos><companies><company><id>{}</id><name>Pressing Plant</name><catno>\
         </catno><entity_type>17</entity_type><entity_type_name>Pressed By</entity_type_name>\
         <resource_url>https://example.com</resource_url></company></companies></release>",
        if id.is_multiple_of(3) {
            format!("<identifier type=\"Barcode\" value=\"50{id:011}\"/>")
        } else {
            String::new()
        },
        id % labels + 1
    );
    xml
}
