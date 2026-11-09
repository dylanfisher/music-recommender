//! The golden cases both repos check (ADR 0010): `fixtures/rank-golden.json`, committed here
//! and in the player, whose SHA-256 is recorded in ADR 0010 and the player's ADR 0140, so an
//! edit on one side fails until both change.

use std::fmt::Write as _;

use catalog::download::{Policy, Track};
use features::rank::{self, File, Peer, Release};
use primitives::{fold, similarity};
use serde_json::Value;

/// `rank-golden.json`'s SHA-256, as ADR 0010 records it.
const GOLDEN_SHA256: &str = "c9f9134c5460472361fbd2005fbee149d3630832caf1f4706c57ce5eaaddf2d2";

const GOLDEN: &[u8] = include_bytes!("fixtures/rank-golden.json");

fn golden() -> Value {
    serde_json::from_slice(GOLDEN).unwrap()
}

fn cases<'a>(golden: &'a Value, section: &str) -> &'a Vec<Value> {
    golden[section].as_array().unwrap()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-12
}

#[test]
fn the_file_is_the_one_both_repos_recorded() {
    assert_eq!(sha256_hex(GOLDEN), GOLDEN_SHA256);
}

#[test]
fn the_thresholds_are_the_player_s() {
    let thresholds = &golden()["thresholds"];
    let expected = [
        ("same_title", similarity::SAME_TITLE),
        ("contained_title", similarity::CONTAINED_TITLE),
        ("paired_title", similarity::PAIRED_TITLE),
        ("likely_tracklist", similarity::LIKELY_TRACKLIST),
        ("duration_seconds", similarity::DURATION_SECONDS),
        ("duration_fraction", similarity::DURATION_FRACTION),
        ("duration_weight", similarity::DURATION_WEIGHT),
    ];
    for (name, value) in expected {
        assert_eq!(thresholds[name].as_f64(), Some(value), "{name}");
    }
    assert_eq!(
        thresholds["contained_letters"].as_u64(),
        Some(similarity::CONTAINED_LETTERS as u64)
    );
}

#[test]
fn the_table_is_the_fold_s() {
    let golden = golden();
    let table: Vec<(String, String)> = cases(&golden, "table")
        .iter()
        .map(|row| {
            (
                row[0].as_str().unwrap().to_string(),
                row[1].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected: Vec<(String, String)> = fold::TABLE
        .iter()
        .map(|(base, letters)| ((*base).to_string(), (*letters).to_string()))
        .collect();
    assert_eq!(table, expected);
    assert_eq!(golden["edition_words"].as_str(), Some(fold::EDITION_WORDS));
}

#[test]
fn every_fold_case() {
    let golden = golden();
    for case in cases(&golden, "fold") {
        let text = case["text"].as_str().unwrap();
        let title = case["title"].as_bool().unwrap();
        assert_eq!(
            fold::fold(text, title),
            case["words"].as_str().unwrap(),
            "{text:?}"
        );
        assert_eq!(
            fold::key(text, title),
            case["key"].as_str().unwrap(),
            "{text:?}"
        );
    }
}

#[test]
fn every_similarity_case() {
    let golden = golden();
    for case in cases(&golden, "similarity") {
        let (a, b) = (case["a"].as_str().unwrap(), case["b"].as_str().unwrap());
        let expected = case["similarity"].as_f64().unwrap();
        let found = similarity::title_similarity(a, b);
        assert!(
            close(found, expected),
            "{a:?} {b:?}: {found} against {expected}"
        );
    }
}

#[test]
fn every_score_case() {
    let golden = golden();
    for case in cases(&golden, "score") {
        let name = case["name"].as_str().unwrap();
        let files: Vec<similarity::File<'_>> = case["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| similarity::File {
                title: file["title"].as_str().unwrap(),
                seconds: file["seconds"].as_f64(),
            })
            .collect();
        let tracks: Vec<similarity::Track<'_>> = case["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|track| similarity::Track {
                title: track["title"].as_str().unwrap(),
                seconds: track["seconds"].as_u64().map(|s| u32::try_from(s).unwrap()),
            })
            .collect();
        let score = similarity::Score::new(&files, &tracks);
        let mapping: Vec<Option<usize>> = case["mapping"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m.as_u64().map(|m| usize::try_from(m).unwrap()))
            .collect();
        assert_eq!(score.mapping, mapping, "{name}");
        assert_eq!(
            Some(score.same_count),
            case["same_count"].as_bool(),
            "{name}"
        );
        assert_eq!(
            Some(score.titles_in_order),
            case["titles_in_order"].as_bool(),
            "{name}"
        );
        assert!(
            close(score.tracklist, case["tracklist"].as_f64().unwrap()),
            "{name}: {}",
            score.tracklist
        );
        assert_eq!(
            Some(score.durations_compared as u64),
            case["durations_compared"].as_u64(),
            "{name}"
        );
        assert_eq!(
            Some(score.durations_agreeing as u64),
            case["durations_agreeing"].as_u64(),
            "{name}"
        );
    }
}

#[test]
fn every_ranking_case() {
    let golden = golden();
    let ranking = cases(&golden, "ranking");
    assert!(ranking.len() >= 14, "{}", ranking.len());
    for case in ranking {
        let name = case["name"].as_str().unwrap();
        let release = &case["release"];
        let tracklists: Vec<Vec<Track>> = release["tracklists"]
            .as_array()
            .unwrap()
            .iter()
            .map(|list| {
                list.as_array()
                    .unwrap()
                    .iter()
                    .map(|track| Track {
                        position: track["position"].as_str().unwrap().to_string(),
                        title: track["title"].as_str().unwrap().to_string(),
                        seconds: track["seconds"].as_u64().map(|s| u32::try_from(s).unwrap()),
                    })
                    .collect()
            })
            .collect();
        let peers: Vec<Peer> = case["peers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|peer| Peer {
                username: peer["username"].as_str().unwrap().to_string(),
                free_slot: peer["free_slot"].as_bool().unwrap(),
                queue_length: peer["queue_length"].as_u64().unwrap(),
                // The golden file is shared with the player (ADR 0010) and has no speeds; one
                // speed for every peer leaves its expected orders as they are.
                upload_speed: 0,
                files: peer["files"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|file| {
                        let number =
                            |key: &str| file[key].as_u64().map(|n| u32::try_from(n).unwrap());
                        File {
                            path: file["path"].as_str().unwrap().to_string(),
                            size: file["size"].as_u64().unwrap(),
                            length: number("length"),
                            bitrate: number("bitrate"),
                            bit_depth: number("bit_depth"),
                            sample_rate: number("sample_rate"),
                            variable_bitrate: file["variable_bitrate"].as_bool(),
                            locked: file["locked"].as_bool().unwrap_or(false),
                        }
                    })
                    .collect(),
            })
            .collect();
        let policy = Policy::parse(case["policy"].as_str().unwrap()).unwrap();
        let found = rank::rank(
            &Release {
                artist: release["artist"].as_str().unwrap(),
                title: release["title"].as_str().unwrap(),
                year: release["year"].as_i64().map(|y| i32::try_from(y).unwrap()),
                tracklists: &tracklists,
            },
            &peers,
            policy,
        );
        let expected = &case["expected"];
        match (&found, expected["reason"].as_str()) {
            (Err(reason), Some(expected)) => assert_eq!(reason, expected, "{name}"),
            (Ok(candidates), None) => {
                let got: Vec<Value> = candidates
                    .iter()
                    .map(|candidate| {
                        serde_json::json!({
                            "peer": candidate.peer,
                            "format": candidate.format.name(),
                            "tracklist": candidate.tracklist,
                            "files": candidate.files.iter().map(|file| serde_json::json!({
                                "remote": file.remote,
                                "name": file.name,
                                "position": file.position,
                            })).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                assert_eq!(&Value::Array(got), &expected["accepted"], "{name}");
            }
            _ => panic!("{name}: found {found:?}, expected {expected}"),
        }
    }
}

/// SHA-256 (FIPS 180-4), written here so the check needs no crate.
#[expect(
    clippy::many_single_char_names,
    clippy::too_many_lines,
    reason = "the standard's names, in the standard's order"
)]
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a_2f98,
        0x7137_4491,
        0xb5c0_fbcf,
        0xe9b5_dba5,
        0x3956_c25b,
        0x59f1_11f1,
        0x923f_82a4,
        0xab1c_5ed5,
        0xd807_aa98,
        0x1283_5b01,
        0x2431_85be,
        0x550c_7dc3,
        0x72be_5d74,
        0x80de_b1fe,
        0x9bdc_06a7,
        0xc19b_f174,
        0xe49b_69c1,
        0xefbe_4786,
        0x0fc1_9dc6,
        0x240c_a1cc,
        0x2de9_2c6f,
        0x4a74_84aa,
        0x5cb0_a9dc,
        0x76f9_88da,
        0x983e_5152,
        0xa831_c66d,
        0xb003_27c8,
        0xbf59_7fc7,
        0xc6e0_0bf3,
        0xd5a7_9147,
        0x06ca_6351,
        0x1429_2967,
        0x27b7_0a85,
        0x2e1b_2138,
        0x4d2c_6dfc,
        0x5338_0d13,
        0x650a_7354,
        0x766a_0abb,
        0x81c2_c92e,
        0x9272_2c85,
        0xa2bf_e8a1,
        0xa81a_664b,
        0xc24b_8b70,
        0xc76c_51a3,
        0xd192_e819,
        0xd699_0624,
        0xf40e_3585,
        0x106a_a070,
        0x19a4_c116,
        0x1e37_6c08,
        0x2748_774c,
        0x34b0_bcb5,
        0x391c_0cb3,
        0x4ed8_aa4a,
        0x5b9c_ca4f,
        0x682e_6ff3,
        0x748f_82ee,
        0x78a5_636f,
        0x84c8_7814,
        0x8cc7_0208,
        0x90be_fffa,
        0xa450_6ceb,
        0xbef9_a3f7,
        0xc671_78f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut message = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_be_bytes());
    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (state, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *state = state.wrapping_add(value);
        }
    }
    h.iter().fold(String::new(), |mut hex, word| {
        write!(hex, "{word:08x}").unwrap();
        hex
    })
}

#[test]
fn sha256_matches_the_standard_s_examples() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        sha256_hex(&[b'a'; 1_000]),
        "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3"
    );
}
