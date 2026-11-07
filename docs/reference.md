# Reference sources

Where to look things up about the language, the toolchain and the frameworks this project uses,
and which source answers which kind of question. Open only the source a question needs; don't
read these ahead of time. Local sources first: they match the installed versions, and they don't
cost a web fetch.

Cloned sources live in `tmp/docs/`, which git ignores. Run the commands in
[Getting the clones](#getting-the-clones) once per checkout. If `tmp/docs/` is missing, say so
rather than answering from memory.

## Which source to use

| Question | Source | Path |
|---|---|---|
| How a language feature works | The Rust Reference | `tmp/docs/reference/src/` |
| What a std item does, in the pinned version | std source (the `rust-src` component) | `$(rustc --print sysroot)/lib/rustlib/src/rust/library/` |
| What a crate does, in the locked version | The crate's source | `~/.cargo/registry/src/*/<crate>-<version>/` |
| A Cargo manifest key, workspace or alias setting | The Cargo book | `tmp/docs/cargo/src/doc/src/` |
| Why clippy flagged something, and its config keys | Clippy's book and lint sources (each lint's docs are on its declaration) | `tmp/docs/rust-clippy/book/src/`, `tmp/docs/rust-clippy/clippy_lints/src/` |
| How to shape a public API | Rust API Guidelines | `tmp/docs/api-guidelines/src/` |
| The Discogs dumps' shape and counts | The measured notes in the design doc | [recommendations.md](recommendations.md#the-dumps) |
| A Discogs API endpoint, its reply or its rate limit | The design's summary, then the API docs (one page) | [recommendations.md](recommendations.md#the-community-sync), `https://www.discogs.com/developers` |
| Anything not here | Web, one page at a time | `https://doc.rust-lang.org/`, `https://docs.rs/<crate>/<version>/` |

Search before reading: `ls` or `grep -ril '<term>'` the folder, then open the one file.

## Getting the clones

Shallow, sparse clones, so only the docs come down.

```sh
mkdir -p tmp/docs && cd tmp/docs
clone() {
  git clone -q --depth 1 --filter=blob:none --sparse "$1" "$(basename "$1" .git)" &&
    git -C "$(basename "$1" .git)" sparse-checkout set "${@:2}"
}
clone https://github.com/rust-lang/reference.git src
clone https://github.com/rust-lang/cargo.git src/doc/src
clone https://github.com/rust-lang/rust-clippy.git book/src clippy_lints/src
clone https://github.com/rust-lang/api-guidelines.git src
```

To update them all: `for d in tmp/docs/*/; do git -C "$d" pull -q --depth 1; done`

## Adding a source

Add a row to the table and a line to the clone block in the same change. A source belongs here
when it answers questions this project keeps asking and its license allows the clone.
