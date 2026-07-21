# Design note 0128: `ociman pull`/`ociman push --tls-verify`

Status: implemented
Scope: `bin/ociman/src/main.rs` (`Command::Pull`/`Command::Push` gain
`--tls-verify`, `registry_client` new helper, `cmd_pull`/`cmd_push`
updated); `tests/tests/ociman_tls_verify.rs` (new, 4 tests).

## Closing the gap 0127 explicitly flagged

0127's own "what this doesn't do yet" section named this directly:
"no insecure (plain HTTP) registry support in the `ociman` CLI itself
— `oci_registry::Client::with_options`'s own `insecure_hosts`
parameter already exists... but nothing in `ociman`'s own CLI wires a
`--tls-verify`-equivalent flag to it yet." Picked back up here,
closing it for both `pull` and `push`.

## Matches real `docker`/`podman`'s own `--tls-verify` exactly, checked directly

`podman pull --help`/`podman push --help` (real, installed binary):
`--tls-verify` — "Require HTTPS and verify certificates when
contacting registries (default true)". A boolean flag supporting
several real invocation forms real podman also accepts — bare
`--tls-verify` (true), `--tls-verify=false`, `--tls-verify false` —
implemented via clap's own `num_args = 0..=1` + `default_missing_value
= "true"` + `ArgAction::Set` idiom for exactly this "flag that can
also take an explicit value" shape.

`registry_client` (new, shared by both `cmd_pull`/`cmd_push`): when
`tls_verify` is `false`, adds *only* the one registry host actually
being talked to into `Client::with_options`'s own `insecure_hosts` set
— matching real podman's own scoped-to-the-one-registry behavior, not
a blanket "every registry is insecure" toggle that could otherwise
silently weaken security for an unrelated registry a multi-platform
index redirect might also touch.

## Verified end to end against a real local plain-HTTP registry, not assumed

Before writing any automated test: started a real local `registry:2`
container, pushed a real image into it with real `docker push`, then:

* `ociman pull localhost:15000/test/busybox:latest` (default,
  `--tls-verify` omitted) — a real, clear failure (attempts HTTPS
  against an HTTP-only registry).
* `ociman pull --tls-verify=false localhost:15000/test/busybox:latest`
  — succeeds, digest matches exactly what `docker push` reported.
* `ociman push --tls-verify=false` of a re-tagged copy back to the
  same real registry — succeeds; `curl .../tags/list` against the real
  registry afterward shows both tags present.

## Real, automated tests

Four new CLI-level integration tests in `tests/tests/
ociman_tls_verify.rs`, each against a real, local, anonymous plain-
HTTP mock server (the exact same style `oci_registry::pull`/`push`'s
own library-level tests already establish, reused here specifically
to prove the CLI flag reaches the client, not to re-test the pull/push
*protocol* itself, which those existing tests already cover
thoroughly): `pull --tls-verify=false` succeeding against a real
listening HTTP mock; `pull` (default) failing against the same mock
(HTTPS attempted, refused); `push --tls-verify=false` succeeding
against a second mock implementing just enough of the real `HEAD`/
`POST`/`PUT` push protocol; `push` (default) failing the same way pull
does. All pre-existing `ociman`/`oci-registry` tests still pass
unmodified. Full `cargo build --workspace --locked`/`cargo test
--workspace --locked` (2 clean runs)/`cargo fmt --all --check`/`cargo
clippy --workspace --all-targets --locked -- -D warnings` all clean.

## What this doesn't do yet

* `ociman build`'s own `FROM`/`COPY --from=<external-image>` pulls
  (`resolve_or_pull`) still always assume HTTPS — this increment only
  covers the two commands that talk to a registry *directly and
  explicitly* (`pull`/`push`); wiring the same flag into `build` is a
  small, separate, well-scoped future increment if it turns out to
  matter in practice (a Containerfile referencing a local/private
  HTTP-only base image is a real, if less common, case than `pull`/
  `push` against one directly).
* No `--cert-dir` (custom TLS certificates) — real podman's own
  additional flag for a registry with a private CA, not needed for
  the common "plain HTTP, no TLS at all" local/dev-registry case this
  increment targets.
