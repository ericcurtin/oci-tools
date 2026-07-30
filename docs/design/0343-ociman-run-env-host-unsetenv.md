# Design note 0343: `ociman run`/`create --env-host`/`--unsetenv`/`--unsetenv-all`

Status: implemented
Scope: `bin/ociman/src/main.rs`, `tests/tests/ociman_run.rs`.

## What this closes

Three more real `podman run`/`create` environment flags with no
equivalent in this project before now: `--env-host` (layer this
process's own live environment into the container), `--unsetenv`
(remove one named default/image-declared variable), and
`--unsetenv-all` (remove all of them). `podman exec` has none of the
three (checked directly, `~/git/podman/cmd/podman/containers/
exec.go`) — this is a `run`/`create`-only feature, same scope
`0341`'s `--env-file` established.

## Real, checked-directly semantics — and a genuine surprise

Read real podman's own `CompleteSpec` directly (`~/git/podman/pkg/
specgen/generate/container.go`), not guessed from `--help` text. The
naive assumption going in was "the image's own declared env always
wins over `--env-host`'s host passthrough" — checking the actual
source proved that assumption **wrong**:

```go
for _, e := range s.UnsetEnv {
    delete(defaultEnvs, e)
}
if s.UnsetEnvAll != nil && *s.UnsetEnvAll {
    defaultEnvs = make(map[string]string)
}
osEnv := envLib.Map(os.Environ())
if envHost {
    defaultEnvs = envLib.Join(defaultEnvs, osEnv)
}
...
s.Env = envLib.Join(defaultEnvs, s.Env)
```

`--unsetenv`/`--unsetenv-all` run first, against whatever's
accumulated so far (built-in defaults + the image's own declared env).
Then — **unconditionally**, regardless of what `--unsetenv`/
`--unsetenv-all` just did — `--env-host` is applied, and `Join(base,
override)` always lets `override` win. Two real, checked-directly
consequences neither obvious from the flag names alone:

1. `--env-host` **wins over the image's own declared value** for a
   shared key (e.g. `PATH`), not merely "fills in whatever the image
   didn't declare." A container built `FROM` an image with its own
   carefully-set `PATH` still gets the *host's* live `PATH` if
   `--env-host` is given.
2. `--env-host` **revives** a variable `--unsetenv`/`--unsetenv-all`
   just removed, if this process's own live environment happens to
   have it set — so `podman run --unsetenv-all --env-host` does
   **not** produce an empty environment; it produces the *entire* live
   host environment, undoing `--unsetenv-all`'s own effect entirely
   for any key the host has set.

Only an explicit `-e`/`--env`/`--env-file` (`0341`, already the last,
always-winning layer) can override `--env-host` itself.

## Design decision: port the quirk verbatim, don't "fix" it

Given this project's own established precedent for preserving a real,
even-surprising upstream behavior exactly rather than silently
"improving" it when the two disagree (e.g. `0327`/`0330`'s icon-path
and `--extra-flags` quirks), both surprises above are ported exactly
as real podman implements them, documented prominently in the new
flags' own doc comments and covered by dedicated tests
(`run_env_host_flag_layers_in_the_live_environment_and_wins_over_the_image`,
`run_unsetenv_all_does_not_defeat_env_host_applied_after_it`) rather
than treated as bugs to correct.

## Implementation

Three new `RunArgs` fields (shared by `Run`/`Create` via the existing
`#[command(flatten)]`, so both commands get all three for free):
`env_host: bool` (`--env-host`), `unsetenv: Vec<String>`
(`--unsetenv`, repeatable), `unsetenv_all: bool` (`--unsetenv-all`).

`synthesize_spec` (already `#[allow(clippy::too_many_arguments)]`)
gained three new parameters, applied in the real, checked-directly
order — right after the existing built-in-default-or-image-env
computation, and *before* `apply_env_overrides` (which stays last,
unconditional, and unchanged — `-e`/`--env-file` still always wins):

```rust
if !unsetenv.is_empty() {
    process.env.retain(|entry| {
        let key = entry.split('=').next().unwrap_or(entry.as_str());
        !unsetenv.iter().any(|name| name == key)
    });
}
if unsetenv_all {
    process.env.clear();
}
if env_host {
    for (key, value) in std::env::vars() {
        build::set_env_var(&mut process.env, &key, &value);
    }
}
build::apply_env_overrides(&mut process.env, env);
```

Reuses `set_env_var` (already `pub(crate)`, `0341`'s own established
"replace an already-present key in place, otherwise append"
semantics) for `--env-host`'s own per-variable win — no new
primitive needed.

One deliberate scope boundary: real podman's own *first* `--env-host`
application (inside `GetDefaultEnvEx`, folded into `defaultEnvs`
*before* the image's own env is joined in) is not modeled separately,
since it's provably moot whenever an image is actually resolved (the
image env join, then the second unconditional `Join(defaultEnvs,
osEnv)` at the very end, together make the first application's own
effect completely overwritten either way) — and `ociman run`/`create`
always resolves an image, unlike real podman's own separate
`--rootfs`-based, image-less path this project has no equivalent of.
Real `containers.conf`-sourced defaults (`GetDefaultEnvEx`'s own
`c.Containers.Env`) and `--env-merge` are both out of scope entirely —
this project has no `containers.conf` equivalent, and `--env-merge`
needs a real Dockerfile-word-expansion engine this feature doesn't
otherwise need.

## Verified

New integration tests in `ociman_run.rs`:
`run_env_host_flag_layers_in_the_live_environment_and_wins_over_the_image`,
`run_without_env_host_flag_the_images_own_env_is_untouched_by_the_live_host`
(a plain control), `run_unsetenv_flag_removes_a_named_variable_the_image_declared`,
`run_unsetenv_all_flag_clears_every_default_but_explicit_env_still_wins`,
`run_unsetenv_all_does_not_defeat_env_host_applied_after_it`.

One real testing subtlety found and worked around, not silently
avoided: a `PATH=[]` assertion after `--unsetenv-all` is a **false
negative** with a real shell — busybox's own `ash` (confirmed
directly, along with the host's own `/bin/sh`) auto-populates its own
fallback `PATH` internally whenever it's genuinely absent from its
process environment at all, a real property of the shell itself, not
of this container's own spec. `run_unsetenv_all_flag_clears_every_
default_but_explicit_env_still_wins` checks `FOO`/`BAZ` (ordinary,
non-magic names) instead, documented in its own doc comment.

Full workspace: `cargo build --workspace --locked`, `cargo fmt --all
--check`, `cargo clippy --workspace --all-targets --locked -- -D
warnings`, `cargo test --workspace --locked` (113 test-result blocks,
0 failures), `python3 ci/guards.py`, `cargo deny check`.

## Still ahead

`ocibox create`/`enter --hostname` (an override on top of `0292`'s
already-correct-but-diverging-from-real-distrobox default) remains a
separate, similarly-small, not-yet-scoped candidate. Real podman's own
`--env-merge` (Dockerfile-word-expansion-based env substitution) and
`containers.conf`-sourced default env remain deliberately out of scope
for the reasons given above.
