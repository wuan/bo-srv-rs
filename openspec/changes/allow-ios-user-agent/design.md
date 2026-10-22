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

The Blitzortung iOS app (wuan/bo-ios) sends `User-Agent: bo-ios` with no
version, while the Android app sends `bo-android-<versionCode>`. A bare
`bo-ios` parses to version `0` under the current rule, so the version-as-validity
representation cannot express "valid iOS client without a version".

## Goals / Non-Goals

**Goals:**

- Accept the iOS client with its real `bo-ios` user agent, and also accept a
  future versioned `bo-ios-<n>` form, without weakening the check for unknown
  clients.
- Keep the Android behaviour byte-for-byte: same accepted strings, same version
  parsing, same platform marker, same gzip downgrade.

**Non-Goals:**

- Changing the iOS app's user agent (tracked in wuan/bo-ios, not here).
- Any JSON-RPC, CLI-flag, database-schema or response-shape change.
- Rewriting historical usage-log rows.

## Decisions

### Represent a client as a kind plus an optional version

Replace the "version or 0" representation with a parsed client kind:

```
enum UserAgentClient { Android(i64), Ios(Option<i64>) }
```

`bo-android-<n>` maps to `Android(n)` only when `n` parses and is non-zero
(unchanged: `bo-android`, `bo-android-`, `bo-android-abc` and `bo-android-0`
stay invalid). `bo-ios` maps to `Ios(None)`, `bo-ios-<n>` to `Ios(Some(n))`
when `n` parses. Anything else is `None`.

*Alternative considered:* accept any user agent that starts with a known
prefix. Rejected: it would newly allow `bo-android-abc` and `bo-android-`
(blocked today) and would lose the numeric version the gzip downgrade and the
usage log need.

### Validity is "a recognised client", not "version non-zero"

`forbidden_client_reason` blocks when the parsed client is `None`, instead of
when the version is `0`. Scenario "Unknown user agents stay blocked" and the
existing Android rejections are preserved by the parser above.

### The platform marker gains `I`

`client_platform` returns `A` for `Android(_)`, `I` for `Ios(_)` and `-`
otherwise. The usage-log platform column is an opaque string in
`stats::parse`, so `I` is backward compatible; only the documentation changes.

### The gzip downgrade keeps reading the Android version

`fix_bad_accept_header` strips `Accept-Encoding` only for
`Android(v)` with `1 <= v <= MAX_COMPATIBLE_ANDROID_VERSION`; iOS clients are
never downgraded. The bug is an Android client bug and iOS does not send
`Accept-Encoding` itself.

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
  version-non-zero rule; unit tests assert `bo-android`, `bo-android-abc` and
  `bo-android-0` still block.
- **Prefix check matches too much** (e.g. `bo-iosfoo`) → match on the parsed
  first token and require either the exact `bo-ios` word or a
  `bo-ios-<integer>` split, never a bare `starts_with`.
- **Usage-log consumers assume only `A`/`-`** → the column is already read as
  an opaque string; document `I` in README and the parser comment.
- **A future iOS version suffix could be non-numeric** → a non-integer suffix is
  treated as an unknown client (blocked), matching the Android rule; the iOS
  app can adopt `bo-ios-<int>` later without another server change.

## Migration Plan

No schema or persisted-format change. Deploy the service and iOS requests start
being served; usage rows for iOS clients carry platform `I`. Rollback is
reverting the build; previously written rows stay readable.

## Open Questions

1. The current iOS app sends bare `bo-ios`; this change also accepts
   `bo-ios-<n>` so the app can adopt versioning later. If the maintainer prefers
   iOS to be version-only, the spec and parser would tighten — confirm before
   archive.
2. Platform marker `I` is this design's choice for the usage-log column.
   Confirm the marker (or prefer `-`) before archive; only the spec/tests
   change either way.
