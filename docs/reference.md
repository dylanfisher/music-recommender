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
| {{How a language feature works}} | {{The language's book or reference}} | {{`tmp/docs/<repo>/<docs dir>/`}} |
| {{What a library function does, in the installed version}} | {{Installed package source or type stubs}} | {{e.g. `node_modules/<pkg>/`, `.venv/lib/.../site-packages/<pkg>/`, `$(go env GOMODCACHE)`}} |
| {{A formatter or lint result from `./scripts/check`}} | {{The tool's rule docs}} | {{`tmp/docs/<tool>/docs/`}} |
| {{Anything not cloned}} | {{Web, one page at a time}} | {{URL pattern}} |

Search before reading: `ls` or `grep -ril '<term>'` the folder, then open the one file.

## Getting the clones

Shallow, sparse clones, so only the docs come down.

```sh
mkdir -p tmp/docs && cd tmp/docs
clone() {
  git clone -q --depth 1 --filter=blob:none --sparse "$1" "$(basename "$1" .git)" &&
    git -C "$(basename "$1" .git)" sparse-checkout set "${@:2}"
}
# {{clone https://github.com/<org>/<repo>.git <docs dir>}}
```

To update them all: `for d in tmp/docs/*/; do git -C "$d" pull -q --depth 1; done`

## Adding a source

Add a row to the table and a line to the clone block in the same change. A source belongs here
when it answers questions this project keeps asking and its license allows the clone.
