# Private crate registry (JFrog Artifactory)

`package.publish` is `false` and this crate is not on crates.io, so for
eighteen releases there has been no way for anyone else to depend on it. This
page describes the private Cargo registry that fixes that, set up as part of
[Lab271's JFrog evaluation](https://github.com/Lab271/labs-jfrog-poc). It is
**opt-in**: nothing in a default clone points at it, and `cargo build` on a
fresh clone or a fork resolves from crates.io exactly as before. See
[ADR-0038](adr/0038-cargo-registry-opt-in-not-committed.md) for why it must
stay that way.

## Repositories

| Key | Kind | Contents |
|---|---|---|
| `lab-cargo-dev` | virtual | everything: dev, prod, and the crates.io cache |
| `lab-cargo-prod` | virtual | promoted releases only — **no remote**, cannot reach the internet |
| `lab-cargo-dev-local` | local | where `sqlite-rs` is published |
| `lab-cargo-prod-local` | local | where a release is promoted to |
| `lab-crates-io-remote` | remote | caching proxy for `index.crates.io` |

The index URL for any of them is
`sparse+https://schubergphilis.jfrog.io/artifactory/api/cargo/<key>/index/`.

## Opting in

```bash
cp .cargo/config.toml.example .cargo/config.toml
export CARGO_REGISTRIES_LAB_CARGO_DEV_TOKEN="<jfrog identity token>"
cargo fetch --locked
```

`.cargo/config.toml` is gitignored. Read the template's comments before
editing it — it explains which of the two mechanisms to use when.

Prefer the environment variable to `~/.cargo/credentials.toml`; it keeps the
token off disk. Nothing in this repository can create a token — a human with
access to the `lab` project has to issue one from the JFrog UI.

`Cargo.lock` is unaffected. After a full `cargo fetch --locked` through
Artifactory all 116 `source` entries still read
`registry+https://github.com/rust-lang/crates.io-index`, because Cargo
requires a replacement source to serve byte-identical crates and verifies each
one against the checksum already in the lockfile. The lockfile stays portable
and public clones keep working.

## Publishing

`publish = false` does **not** have to change. `cargo package` works under it,
and the resulting `.crate` is deployed with the JFrog CLI, which also records
build-info:

```bash
cargo package --locked
jf rt upload target/package/sqlite-rs-<version>.crate \
    "lab-cargo-dev-local/crates/sqlite-rs/sqlite-rs-<version>.crate" \
    --build-name=sqlite-rs --build-number="$N" --project=lab
jf rt build-add-git      sqlite-rs "$N" --project=lab
jf rt build-publish      sqlite-rs "$N" --project=lab
```

`--project=lab` is mandatory on the build-info commands; without it the CLI
targets a platform-level repository and gets a flat 403 whose message names
the repository rather than the missing flag.

Artifactory generates the Cargo index entry from the uploaded `.crate` itself
— parsing `Cargo.toml` for `deps`, `features` and the checksum — a few seconds
after the upload lands. There is no `jf cargo` command and none is needed.

`cargo publish` is the other option, but it requires `package.publish` to name
the registry and it cannot produce build-info. ADR-0038 records why the upload
path is preferred.

### Promoting a release

```bash
jf rt build-promote sqlite-rs "$N" lab-cargo-prod-local \
    --project=lab --status=Released --copy=true
```

`--copy=true` is not optional in practice: **`build-promote` moves by
default**, which would delete the crate from `lab-cargo-dev-local` and break
anything still resolving that version from `lab-cargo-dev`. Record the dev
digest before promoting, because after a move there is nothing left to compare
against.

## Consuming it from another crate

```toml
[dependencies]
sqlite-rs = { version = "0.18", registry = "lab-cargo-prod" }
```

with the matching `[registries.lab-cargo-prod]` block from the template in the
consumer's own `.cargo/config.toml`.

Use this named-registry form, not the source replacement, when depending on
`sqlite-rs`. Under source replacement the consumer's lockfile records

```toml
source = "registry+https://github.com/rust-lang/crates.io-index"
```

for a crate that is not on crates.io at all. That is untrue, and it makes
[`deny.toml`](https://github.com/Lab271/sqlite-rs/blob/main/deny.toml)'s
`sources` check — `unknown-registry = "deny"` with `allow-registry` set to
crates.io only — pass a private-registry dependency without comment. The
named form records the real index URL, and the gate sees it.

## Two things to know before relying on this

**The crate name is already taken on crates.io.** An unrelated `sqlite-rs`
has 19 published versions, up to `0.3.7`. `lab-cargo-dev` merges its crates.io
cache with our local repository, so that one index path serves 20 versions
belonging to two different projects, with nothing in the metadata
distinguishing them. Our `0.18.10` resolves correctly today only because the
version ranges do not overlap. `lab-cargo-prod` has no remote and serves
`0.18.10` alone, which is why release consumers should point there. Renaming
the crate is the real fix and should happen before any public release.

**Xray does not scan this crate's dependency graph.** `jf audit` on the source
tree reports the project as `[unknown]` and generates an SBOM with *no library
components* — it does not recognise `Cargo.toml`/`Cargo.lock` as a dependency
manifest at all, so the 116-crate closure that `make check-deny` and
`sqlite-rs-dev.cdx.json` already cover is never examined. Xray does have Cargo
CVE data and does apply it to a `.crate` artifact scanned with `jf scan`
(confirmed against a deliberately vulnerable `time 0.1.44`, which reports
`CVE-2020-26235`, type `cargo`), but this crate has zero runtime dependencies,
so there is nothing there for it to find. `make check-deny` remains the
supply-chain gate; Artifactory adds distribution, not assurance.
