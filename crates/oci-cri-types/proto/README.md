# Vendored CRI protobuf definitions

`api.proto` is an unmodified copy of the real Kubernetes CRI v1 API
definition (`k8s.io/cri-api/pkg/apis/runtime/v1/api.proto`), vendored
from `cri-o`'s own vendor tree
(`vendor/k8s.io/cri-api/pkg/apis/runtime/v1/api.proto`) — the exact
same schema real `cri-o`/`containerd` implement and real `kubelet`/
`crictl` speak against, so `ocicri` is a genuine drop-in CRI
implementation rather than an invented approximation of the protocol.

Licensed Apache License 2.0 by The Kubernetes Authors (see the file's
own header) — compatible with this project's own Apache-2.0 license.

One small, documented deviation from the real upstream file: the four
`[debug_redact = true]` field options on `AuthConfig` are stripped
(see the comment directly above that message in `api.proto`) — this
project's own build-time `protoc` predates the protobuf release that
added `debug_redact` as a real `google.protobuf.FieldOptions` field,
so it fails to parse the file with them present. Removing them changes
nothing about the wire format, field numbers, or field types (it only
controls a debug/log-redaction hint), so every message this crate
generates stays byte-for-byte wire-compatible with the real, unmodified
upstream schema either way.
