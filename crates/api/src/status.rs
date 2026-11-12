//! `GET /status`: the owner's status page, HTML for a browser on the LAN, open without a
//! sign-in (ADR 0031). It shows `features::status`, the numbers
//! `sync status` prints, one section for each part of the service, the scores included.

use config::Config;
use features::clock::Clock;
use features::status::{self, RequestKind, Status, Tier};

use crate::reply::Reply;

/// The page's path, the only one a GET reaches.
pub(crate) const PATH: &str = "/status";

/// How often the page reloads itself, in seconds.
const REFRESH_SECONDS: u32 = 30;

/// Answers `GET /status` with the page.
pub(crate) fn answer(config: &Config, clock: &dyn Clock) -> Reply {
    let (code, body) = match status::read(config, clock) {
        Ok(status) => (200, sections(&status)),
        Err(error) => (
            500,
            format!(
                "<p>The status couldn't be read: {}</p>\n",
                escape(&error.to_string())
            ),
        ),
    };
    Reply::html(code, vec![], document(&body))
}

/// A whole page around `body`, reloading every `REFRESH_SECONDS`.
fn document(body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"refresh\" content=\"{REFRESH_SECONDS}\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>music-recommender</title>\n<style>\n\
         body {{ font: 15px/1.5 -apple-system, sans-serif; margin: 1.5em; max-width: 48em; }}\n\
         table {{ border-collapse: collapse; }}\n\
         th, td {{ padding: 0.2em 1em 0.2em 0; text-align: left; }}\n\
         td {{ font-variant-numeric: tabular-nums; }}\n\
         dt {{ font-weight: bold; }}\n\
         </style>\n</head>\n<body>\n<h1>music-recommender</h1>\n{body}</body>\n</html>\n"
    )
}

/// One section for each part of the service, written as HTML.
fn sections(status: &Status) -> String {
    community_sync(status) + &dumps(status) + &scores(status)
}

/// The scores' section: the list in use and whether a rebuild runs.
fn scores(status: &Status) -> String {
    let mut html = String::from("<section id=\"scores\">\n<h2>Scores</h2>\n<dl>\n");
    html += &item("List", &escape(&status.scores_words()));
    html += &item("Rebuilding", &escape(&status.scores.running_words()));
    html += "</dl>\n</section>\n";
    html
}

/// One `<dt>`/`<dd>` pair of a definition list.
fn item(name: &str, value: &str) -> String {
    format!("<dt>{name}</dt><dd>{value}</dd>\n")
}

/// The community sync's section.
fn community_sync(status: &Status) -> String {
    let sync = &status.sync;
    let p = &sync.progress;
    let yes = |flag| if flag { "yes" } else { "no" };
    let mut html = String::from("<section id=\"community-sync\">\n<h2>Community sync</h2>\n<dl>\n");
    html += &item("Paused", yes(p.paused));
    html += &item("Running", yes(sync.running));
    if sync.other_shape {
        html += &item("Database", &escape(status::OTHER_SHAPE));
    }
    if let Some(since) = p.yielding_since {
        html += &item(
            "Yielding to another client",
            &format!("for {}", status.ago(since)),
        );
    }
    html += &item("Requests in the last hour", &sync.requests.to_string());
    let last_error = match &p.last_error {
        Some((at, message)) => format!("{} ago: {}", status.ago(*at), escape(message)),
        None => "none".to_string(),
    };
    html += &item("Last error", &last_error);
    html += &item("Time left", &sync.time_left());
    html += "</dl>\n<table>\n<tr><th></th><th>Queued</th><th>With values</th><th>Failed</th>\
             <th>Gone</th><th>Due for refresh</th></tr>\n";
    html += &[
        ("Library", p.library),
        ("Candidates", p.candidates),
        ("Lists", p.lists),
    ]
    .map(|(name, counts)| {
        format!(
            "<tr><th>{name}</th><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            counts.queued, counts.with_values, counts.failed, counts.gone, counts.due
        )
    })
    .concat();
    html += "</table>\n";
    html += &queue(status);
    if !p.failures.is_empty() {
        html += "<h3>Failed releases, newest first</h3>\n<ul>\n";
        for (release_id, error) in &p.failures {
            html += &[
                "<li>release ",
                &release_id.to_string(),
                ": ",
                &escape(error),
                "</li>\n",
            ]
            .concat();
        }
        html += "</ul>\n";
    }
    html += "</section>\n";
    html
}

/// The queue's part of the sync's section: what is waiting in each tier, the requests the
/// sync sends next, and the ones it fetched last.
fn queue(status: &Status) -> String {
    let queue = &status.sync.queue;
    let mut html = String::from("<h3>Waiting, by tier</h3>\n<table>\n");
    for (tier, count) in &queue.tiers {
        html += &[
            "<tr><th>",
            tier.name(),
            "</th><td>",
            &count.to_string(),
            "</td></tr>\n",
        ]
        .concat();
    }
    html += "</table>\n<h3>Next up</h3>\n";
    if queue.next.is_empty() {
        html += "<p>Nothing never fetched is due; the sync refreshes the oldest values.</p>\n";
    } else {
        html += "<ol>\n";
        for (request, tier, priority) in &queue.next {
            let why = match tier {
                Tier::Library => String::new(),
                // An asked request's priority is when its list was asked for.
                #[expect(clippy::cast_possible_truncation, reason = "a time in seconds")]
                Tier::Asked => format!(", list asked for {} ago", status.ago(*priority as i64)),
                _ => format!(", priority {priority:.3}"),
            };
            html += &["<li>", &link(*request), " (", tier.name(), &why, ")</li>\n"].concat();
        }
        html += "</ol>\n";
    }
    if !queue.recent.is_empty() {
        html += "<h3>Fetched last</h3>\n<ul>\n";
        for fetched in &queue.recent {
            let title = fetched
                .title
                .as_ref()
                .map_or_else(String::new, |title| [": ", &escape(title)].concat());
            let values = fetched
                .values
                .map_or_else(String::new, |(average, count, have)| {
                    format!(", rated {average:.2} by {count}, {have} own it")
                });
            let error = fetched
                .error
                .as_ref()
                .map_or_else(String::new, |error| [", failed: ", &escape(error)].concat());
            html += &[
                "<li>",
                &link(fetched.request),
                &title,
                &values,
                &error,
                " (",
                fetched.tier.name(),
                ", ",
                &status.ago(fetched.at),
                " ago)</li>\n",
            ]
            .concat();
        }
        html += "</ul>\n";
    }
    html
}

/// A request as a link to its page on Discogs.
fn link(request: status::Request) -> String {
    let path = match request.kind {
        RequestKind::Release => "release",
        RequestKind::ArtistReleases => "artist",
        RequestKind::MasterVersions => "master",
    };
    format!(
        "<a href=\"https://www.discogs.com/{path}/{}\">{}</a>",
        request.id,
        escape(&request.to_string())
    )
}

/// The dumps' and the import's section: the folder and its newest complete set, whether an
/// import runs, the catalog in use, a build in progress, and each dump's records.
fn dumps(status: &Status) -> String {
    let dumps = &status.dumps;
    let mut html = String::from("<section id=\"dumps\">\n<h2>Dumps and import</h2>\n<dl>\n");
    match &dumps.dir {
        None => html += &item("Dumps folder", "off: the config has no [dumps] table"),
        Some(dir) => {
            html += &item("Dumps folder", &escape(&dir.display().to_string()));
            let newest = match &dumps.newest_set {
                Ok(Some(date)) => escape(date),
                Ok(None) => "none".to_string(),
                Err(error) => format!("the folder can't be read: {}", escape(error)),
            };
            html += &item("Newest complete set", &newest);
        }
    }
    html += &item("Fetch or import running", &escape(&dumps.running_words()));
    html += &item("Catalog", &escape(&status.catalog_words()));
    if let Some(build) = status.build_words() {
        html += &item("Build", &escape(&build));
    }
    html += "</dl>\n";
    let rows = dumps.rows();
    if !rows.is_empty() {
        html += "<table>\n<tr><th>Dump</th><th>Records</th></tr>\n";
        for row in rows {
            html += &[
                "<tr><th>",
                row.dump,
                "</th><td>",
                &row.counts(),
                "</td></tr>\n",
            ]
            .concat();
        }
        html += "</table>\n";
    }
    html += "</section>\n";
    html
}

/// `text` with the characters HTML gives a meaning to written as references, so text
/// from outside (Discogs, the database, an error) shows as text in an element or an
/// attribute.
fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            c => escaped.push(c),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::escape;

    #[test]
    fn escapes_the_characters_html_reads() {
        assert_eq!(
            escape(r#"<a href="x" title='y'>&amp;</a>"#),
            "&lt;a href=&quot;x&quot; title=&#39;y&#39;&gt;&amp;amp;&lt;/a&gt;"
        );
        assert_eq!(escape("plain text, café"), "plain text, café");
    }
}
