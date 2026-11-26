# Vendored OpAMP protocol definitions

`opamp/v1/opamp.proto` and `opamp/v1/anyvalue.proto` are copied verbatim from
the OpenTelemetry **OpAMP specification** repository,
<https://github.com/open-telemetry/opamp-spec>, commit
`61372c6916c4be08eb0409f7e1001e25cedb891f` (`main`, 2026-11-09).

They are Copyright 2021 OpenTelemetry Authors and licensed under the
Apache License, Version 2.0 — not under this repository's AGPL-3.0-only. The
licence header inside each file is the one that applies to it, which is why
these two files carry no SPDX header of ours.

Why vendored rather than generated from: the crate speaks a JSON subset of
OpAMP (`src/lib.rs`), and the test `json_field_names_exist_in_the_proto`
reads these files to prove every field the Rust types name exists on the
corresponding protobuf message. That is the guard against drifting from the
standard until a `prost` build lands (PLAN.md, marco "fim da fase 1"). Update
by copying the files again and recording the new commit here.
