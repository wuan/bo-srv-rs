# Design

## Context

See proposal.md — Why. The service authenticates data requests in
`Service::forbidden_client_reason` (`src/service.rs`) by parsing the
`User-Agent` header: `Request::user_agent_version` returns `i64` and the
request is blocked when it is `0`. `USER_AGENT_PREFIX` is the single constant
`bo-android-`. The usage log derives a platform marker and a version from the
same header through `user_agent_version` / `client_platform`
(`src/service_log.rs`), and `fix_bad_accept_header` strips `Accept-Encoding` for
old Android clients.

The Android app sends `bo-android-<versionCode>`. Review decided the iOS app
should mirror that and send `bo-ios-<int>` (its build number); the current iOS
app still sends bare `bo-ios`. The iOS app change is tracked in wuan/bo-ios and
is out of scope here.

## Goals / Non-Goals

**Goals:**

- Accept the versioned iOS client `bo-ios-<n>` alongside `bo-android-<n>`,
  without weakening the check for unknown clients or the existing Android
  rejections.
- Keep the Android behaviour byte-for-byte: same accepted strings, same version
  parsing, same platform marker, same gzip downgrade.

**Non-Goals:**

- Changing the iOS app's user agent (tracked in wuan/bo-ios, not here). Until it
  sends `bo-ios-<n>`, its bare `bo-ios` requests stay blocked.
- Any JSON-RPC, CLI-flag, database-schema or response-shape change.
- Rewriting historical usage-log rows.

## Decisions

### Represent a client as a kind plus a version

Replace the "version or 0" representation with a parsed client kind:

```
enum UserAgentClient { Android(i64), Ios(i64) }
```

`bo-android-<n>` maps to `Android(n)` and `bo-ios-<n>` to `Ios(n)` only when `n`
parses as a non-zero integer. Bare `bo-ios`, `bo-ios-`, `bo-ios-abc`,
`bo-ios-0`, and the existing Android rejections (`bo-android`, `bo-android-`,
`bo-android-abc`, `bo-android-0`) are all `None`. The version is required for
both clients, so there is no "recognised client without a version".

*Alternative considered:* accept any user agent that starts with a known
prefix. Rejected: it would newly allow `bo-android-abc` and `bo-android-`
(blocked today) and would lose the numeric version the gzip downgrade and the
usage log need.

*Alternative considered:* accept bare `bo-ios` as an unversioned client.
Rejected on review: the iOS form is version-only, mirroring Android, so the
logged client identifies a specific build.

### Validity is "a recognised client"

`forbidden_client_reason` blocks when the parsed client is `None`. Because the
parser only recognises a non-zero integer version, the existing Android
rejections and the bare-`bo-ios` rejection fall out of the same rule and remain
preserved by the scenarios.

### The platform marker gains `I`

`client_platform` returns `A` for `Android(_)`, `I` for `Ios(_)` and `-`
otherwise. The usage-log platform column is an opaque string in `stats::parse`,
so `I` is backward compatible; only the documentation changes.

### The gzip downgrade keeps reading the Android version

`fix_bad_accept_header` strips `Accept-Encoding` only for `Android(v)` with
`1 <= v <= MAX_COMPATIBLE_ANDROID_VERSION`; iOS clients are never downgraded.
The bug is an Android client bug and the iOS app does not send `Accept-Encoding`
itself.

### One parser, shared by validation and logging

`src/service_log.rs` currently duplicates the parsing done in `src/service.rs`.
The iOS addition keeps the duplication small by parsing the same first
whitespace-separated token and applying the same prefix rules; a shared helper
is not required because the two modules own different concerns (request
validation vs. row rendering) and the parsing is a few lines.

*Alternative considered:* move the parser into one shared module. Deferred; it
would widen the diff for no behavioural gain and can be done separately.

## Risks / Trade-offs

- **Silently changing Android behaviour** → the parser keeps the existing
  non-zero-version rule; unit tests assert `bo-android`, `bo-android-abc` and
  `bo-android-0` still block.
- **The current iOS app (bare `bo-ios`) stays blocked** → intended by the
  version-only decision, but it means the server change alone does not unblock
  the shipped app. The iOS app must ship `bo-ios-<n>`; coordinate the two
  releases and document the requirement in README / `doc/jsonrpc_methods.md`.
- **Prefix check matches too much** (e.g. `bo-iosfoo`) → match on the parsed
  first token and require a `bo-ios-<integer>` split, never a bare
  `starts_with`.
- **Usage-log consumers assume only `A`/`-`** → the column is already read as
  an opaque string; document `I` in README and the parser comment.
- **A non-numeric iOS version suffix** → treated as an unknown client
  (blocked), matching the Android rule.

## Migration Plan

No schema or persisted-format change. Deploy the service and roll out the iOS
app build that sends `bo-ios-<n>`; until then the app's bare `bo-ios` requests
stay blocked. Usage rows for iOS clients carry platform `I`. Rollback is
reverting the build; previously written rows stay readable.

## Open Questions

None that change the specs, the approach or the tasks. Review resolved the three
open points from the first draft:

1. The iOS form is **version-only** `bo-ios-<n>`; bare `bo-ios` stays blocked
   (was: accept bare `bo-ios` and optionally a version).
2. Platform marker `I` for iOS in the usage-log column is confirmed.
3. The `Accept-Encoding` downgrade stays Android-only, for Android backwards
   compatibility.

Coordinating the matching `wuan/bo-ios` user-agent change is out of scope for
this repository.
