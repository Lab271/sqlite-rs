# 0038: Artifactory Cargo access is opt-in local config, never a committed source replacement

Date: 2026-09-03

## Context

This crate has shipped eighteen releases and no colleague can depend on it,
because `publish = false` and there is nowhere to publish to. Lab271's JFrog
evaluation ([labs-jfrog-poc#12](https://github.com/Lab271/labs-jfrog-poc/issues/12))
made a private Cargo registry available: `lab-cargo-dev`, a virtual repository
over `lab-cargo-dev-local`, `lab-cargo-prod-local` and a `crates.io` cache.

Cargo has no per-project registry config that lives outside the working tree.
npm has `.npmrc`, and the JFrog CLI can sidestep even that with
`jf npm-config`, which writes `.jfrog/projects/npm.yaml`. **There is no
`jf cargo-config` and no `jf cargo` command at all** — `jf --help` says so
outright: "Cargo has no dedicated 'jf cargo-config' command — it reads the
Artifactory registry directly from `.cargo/config.toml`." So the only
mechanisms Cargo offers are `.cargo/config.toml` (project or user level) and
`CARGO_*` environment variables.

That matters because **this repository is public** and the Artifactory Cargo
index is not anonymously readable. Measured directly:

```
$ curl -o /dev/null -w '%{http_code}' \
    https://schubergphilis.jfrog.io/artifactory/api/cargo/lab-cargo-dev/index/config.json
401
```

The index's own `config.json` also advertises `"auth-required": true`, which
Cargo honours for crate *downloads* as well as index reads. A committed
`.cargo/config.toml` carrying `[source.crates-io] replace-with = ...` would
therefore turn every anonymous `cargo build` — every outside contributor,
every fork, every CI run without the secret — into a 401 on the first
dependency fetch. The same trade-off already came up in the npm pilot of the
same evaluation, where a committed `.npmrc` was rejected for exactly this
reason.

## Decision

Artifactory access is **opt-in and untracked**. The repository commits
`.cargo/config.toml.example` and gitignores `.cargo/config.toml` and
`.cargo/credentials.toml`. A developer who wants the cache copies the
template; everyone else is unaffected and resolves from `crates.io` as
before. No file in the default clone points at Artifactory.

`package.publish` **stays `false`**. Publishing to Artifactory does not
require changing it: `cargo package` works under `publish = false`, and the
resulting `.crate` can be deployed with `jf rt upload`, which Artifactory
indexes into a valid Cargo registry entry on its own (verified: index entry
appeared ~5s after upload, with `deps`, `features` and `cksum` parsed from
`Cargo.toml`). Whether to make this crate publishable at all is a separate,
unmade decision and this ADR does not pre-empt it.

Two consumption forms are documented, and the named-registry form is
preferred for depending on *this* crate:

- `[source.crates-io] replace-with = ...` — for the upstream cache only.
  Cargo enforces byte-identical content in a replacement source and checks
  every `.crate` against the checksum already in `Cargo.lock`, so this is
  provenance-preserving for public crates and leaves `Cargo.lock` portable.
- `sqlite-rs = { version = "0.18", registry = "lab-cargo-dev" }` — for
  depending on this crate. Under source replacement instead, the consumer's
  lockfile records our private crate as
  `source = "registry+https://github.com/rust-lang/crates.io-index"`, which
  is false, and which makes `deny.toml`'s `sources` check
  (`allow-registry = ["https://github.com/rust-lang/crates.io-index"]`,
  `unknown-registry = "deny"`) pass a private-registry dependency silently.
  The named form records the real index URL and the gate sees it.

## Alternatives rejected

- **Commit `.cargo/config.toml` with the source replacement**, as
  [doc 3 of the PoC](https://github.com/Lab271/labs-jfrog-poc/blob/main/docs/03-promotion-and-xray.md)
  suggests per-repo registry config generally. Rejected on the measured 401
  above: it breaks the public contributor path outright, and it breaks it at
  dependency-fetch time with an authentication error that gives an outside
  contributor no hint that the fix is to delete a file they did not add.
- **Set `package.publish = ["lab-cargo-dev"]`.** This does work, and it is
  narrower than it looks — `cargo publish --registry crates-io` still fails
  with "The registry `crates-io` is not listed in the `package.publish`
  value", and a bare `cargo publish` auto-targets the single allowed registry
  ("found `lab-cargo-dev` as only allowed registry"). So it would *not*
  silently open a path to crates.io. Rejected anyway because it is
  unnecessary: the `jf rt upload` path publishes a `publish = false` crate
  fine, and it is the path that also produces build-info
  (`--build-name`/`--build-number`), which `cargo publish` cannot. Keeping
  `publish = false` leaves the crates.io question untouched, which is where it
  belongs. Revisit if we ever want `cargo publish` itself in the release path.
- **A user-level `~/.cargo/config.toml` only, with nothing in the repo.**
  Rejected as undiscoverable: the point of the exercise is that a colleague
  can consume this crate, and a mechanism documented nowhere in the
  repository does not achieve that. The `.example` file is the discoverable
  half; the gitignore is the safety half.
- **Vendoring (`cargo vendor`) instead of a registry.** Rejected: it solves
  offline builds, not distribution. A colleague still could not write
  `sqlite-rs = "0.18"` in their own `Cargo.toml`, which is the actual problem.

## Consequences

- The default clone is unchanged. `cargo build`, `cargo test` and CI on a
  fork resolve from `crates.io` exactly as before; nothing in the tracked
  tree references Artifactory except documentation and one `.example` file.
- Resolving through `lab-cargo-dev` leaves `Cargo.lock` untouched — all 116
  `source` entries still read `registry+https://github.com/rust-lang/crates.io-index`
  after a full `cargo fetch --locked` through the proxy. Verified, and it is a
  guarantee rather than an accident: Cargo refuses a replacement source whose
  checksums differ.
- **`sqlite-rs` is already taken on crates.io** — an unrelated crate, 19
  versions up to 0.3.7. `lab-cargo-dev` merges its cache with our local
  repository, so that index path serves 20 versions from two different
  projects with nothing distinguishing them. Our `0.18.10` resolves correctly
  today only because the version ranges happen not to overlap.
  `lab-cargo-prod` has no remote and serves `0.18.10` alone. Consumers of
  released versions should point at `lab-cargo-prod`; the collision is
  recorded in `docs/src/jfrog-registry.md` and is an argument for renaming the
  crate before any public release.
- Anyone who copies the template still needs a JFrog identity token. Nothing
  in this repository can mint one — see `docs/src/jfrog-registry.md` for what
  a human has to create.
