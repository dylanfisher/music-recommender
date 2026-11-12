//! A reason's words for the command line's lists: one short phrase for each connection, with
//! its names and counts, as the prototype's lists had them (ADR 0026). The API sends them
//! beside the structure itself, so these words are for reading, not parsing.

use crate::score::{Connection, Named};
use crate::seed::Relation;

/// How many of the seeds' roles a credit's words name.
const SEED_ROLES: usize = 2;

/// The words of `connection`: "by Cora Seed, on 2 albums you have".
#[must_use]
pub fn words(connection: &Connection) -> String {
    match connection {
        Connection::Artist { artist, albums } => {
            format!(
                "by {}, on {} you have",
                name(artist, "artist"),
                count(*albums)
            )
        }
        Connection::Credit {
            artist,
            role,
            seed_roles,
            albums,
        } => {
            let who = match role {
                Some(role) => format!("{role}: {}", name(artist, "artist")),
                None => format!("with {}", name(artist, "artist")),
            };
            let roles = if seed_roles.is_empty() {
                "artist".to_string()
            } else {
                seed_roles[..seed_roles.len().min(SEED_ROLES)].join("/")
            };
            format!("{who}, {roles} on {} you have", count(*albums))
        }
        Connection::Label { label, via, albums } => {
            let via = via.as_ref().map_or_else(String::new, |via| {
                format!(" (sublabel or parent of {})", name(via, "label"))
            });
            format!(
                "on {}{via}, near {} you have from it",
                name(label, "label"),
                count(*albums)
            )
        }
        Connection::Related {
            artist,
            relation,
            of,
        } => {
            let (artist, of) = (name(artist, "artist"), name(of, "artist"));
            match relation {
                Relation::Alias => format!("{artist}, an alias of {of}"),
                Relation::Member => format!("{artist}, a member of {of}"),
                Relation::Group => format!("{artist}, a group {of} is in"),
            }
        }
        Connection::Styles { styles } => {
            let styles: Vec<String> = styles.iter().map(|style| name(style, "style")).collect();
            format!("styles you play: {}", styles.join(", "))
        }
    }
}

/// `named`'s name, or its kind and id when the catalog has no name for it.
pub(crate) fn name(named: &Named, kind: &str) -> String {
    name_or_id(named.id, named.name.as_deref(), kind)
}

/// `name`, or the `kind` and `id` of what it names when the catalog has none.
pub(crate) fn name_or_id(id: u64, name: Option<&str>, kind: &str) -> String {
    name.map_or_else(|| format!("{kind} {id}"), str::to_string)
}

/// "1 album" or "N albums".
fn count(albums: usize) -> String {
    if albums == 1 {
        "1 album".to_string()
    } else {
        format!("{albums} albums")
    }
}

#[cfg(test)]
mod tests {
    use super::words;
    use crate::score::{Connection, Named};
    use crate::seed::Relation;

    fn named(id: u64, name: &str) -> Named {
        Named {
            id,
            name: Some(name.to_string()),
        }
    }

    #[test]
    fn an_artist_says_how_many_of_its_albums_you_have() {
        let artist = |albums| Connection::Artist {
            artist: named(20, "Cora Seed"),
            albums,
        };
        assert_eq!(words(&artist(2)), "by Cora Seed, on 2 albums you have");
        assert_eq!(words(&artist(1)), "by Cora Seed, on 1 album you have");
    }

    #[test]
    fn a_credit_names_its_role_and_the_seeds_roles() {
        let credit = Connection::Credit {
            artist: named(21, "Dee Producer"),
            role: Some("Producer".to_string()),
            seed_roles: vec!["Producer".into(), "Mixed By".into(), "Written-By".into()],
            albums: 4,
        };
        assert_eq!(
            words(&credit),
            "Producer: Dee Producer, Producer/Mixed By on 4 albums you have"
        );
        // The candidate's artist, on the seeds only as their artist.
        let with = Connection::Credit {
            artist: named(20, "Cora Seed"),
            role: None,
            seed_roles: Vec::new(),
            albums: 1,
        };
        assert_eq!(words(&with), "with Cora Seed, artist on 1 album you have");
    }

    #[test]
    fn a_label_names_the_seeds_label_it_comes_through() {
        let label = |via| Connection::Label {
            label: named(31, "Seed Sub"),
            via,
            albums: 2,
        };
        assert_eq!(
            words(&label(None)),
            "on Seed Sub, near 2 albums you have from it"
        );
        assert_eq!(
            words(&label(Some(named(30, "Seed Records")))),
            "on Seed Sub (sublabel or parent of Seed Records), near 2 albums you have from it"
        );
    }

    #[test]
    fn a_related_artist_says_how_it_is_related() {
        let related = |relation| Connection::Related {
            artist: named(4, "DJ Ann"),
            relation,
            of: named(2, "Ann Example"),
        };
        assert_eq!(
            words(&related(Relation::Alias)),
            "DJ Ann, an alias of Ann Example"
        );
        assert_eq!(
            words(&related(Relation::Member)),
            "DJ Ann, a member of Ann Example"
        );
        assert_eq!(
            words(&related(Relation::Group)),
            "DJ Ann, a group Ann Example is in"
        );
    }

    #[test]
    fn styles_are_named_and_a_missing_name_is_its_id() {
        let styles = Connection::Styles {
            styles: vec![named(1, "Made-Up Rare"), Named { id: 7, name: None }],
        };
        assert_eq!(words(&styles), "styles you play: Made-Up Rare, style 7");
        let unnamed = Connection::Artist {
            artist: Named { id: 99, name: None },
            albums: 1,
        };
        assert_eq!(words(&unnamed), "by artist 99, on 1 album you have");
    }
}
