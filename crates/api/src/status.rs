//! `GET /status`: the owner's status page, HTML for a browser, signed in with HTTP Basic and
//! the player token as the password (ADR 0021). It shows `features::status`, the numbers
//! `sync status` prints, one section for each part of the service.

use config::Config;
use features::clock::Clock;
use features::status::{self, Status};

use crate::reply::{Reply, WWW_AUTHENTICATE};
use crate::request::Request;
use crate::token;

/// The page's path, the only one a GET reaches.
pub(crate) const PATH: &str = "/status";

/// What a browser without the password is asked to sign in with. The realm names the
/// service in the browser's prompt; UTF-8 lets a password be any text.
const SIGN_IN: &str = r#"Basic realm="music-recommender", charset="UTF-8""#;

/// How often the page reloads itself, in seconds.
const REFRESH_SECONDS: u32 = 30;

/// Answers `GET /status`: the page, or a 401 asking for the password before anything is
/// read.
pub(crate) fn answer(request: &Request, config: &Config, clock: &dyn Clock) -> Reply {
    if !token::signs_in(request.authorization(), config.player_token.secret()) {
        let body = document(
            None,
            "<p>Sign in with the player token as the password; any user name will do.</p>\n",
        );
        return Reply::html(401, vec![(WWW_AUTHENTICATE, SIGN_IN)], body);
    }
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
    Reply::html(code, vec![], document(Some(REFRESH_SECONDS), &body))
}

/// A whole page around `body`, reloading every `refresh` seconds if given.
fn document(refresh: Option<u32>, body: &str) -> String {
    let refresh = refresh.map_or_else(String::new, |seconds| {
        format!("<meta http-equiv=\"refresh\" content=\"{seconds}\">\n")
    });
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n{refresh}\
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
    community_sync(status) + &dumps(status)
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
    html += &[("Library", p.library), ("Candidates", p.candidates)]
        .map(|(name, counts)| {
            format!(
                "<tr><th>{name}</th><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                counts.queued, counts.with_values, counts.failed, counts.gone, counts.due
            )
        })
        .concat();
    html += "</table>\n";
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
