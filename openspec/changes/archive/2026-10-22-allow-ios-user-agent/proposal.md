# Proposal

## Why

The Blitzortung service recognises only the Android client user agent
`bo-android-<int>` and rejects every other request in `is_forbidden` with `{}`,
so the first-party iOS client cannot use the service. Android and iOS are both
Blitzortung clients and should be accepted, logged and counted the same way.

Review resolved the iOS form: like Android, the iOS client identifies itself
with a **versioned** user agent `bo-ios-<int>`, so the recorded client names a
specific build. Bare `bo-ios` (the form the current iOS app sends) therefore
stays blocked; the matching client change lives in the iOS app (wuan/bo-ios) and
is out of scope here.

## What Changes

- Accept requests whose `User-Agent` first whitespace-separated word is the
  versioned iOS client `bo-ios-<int>`, in addition to the existing
  `bo-android-<int>` client. The version suffix is **required**, exactly as for
  Android: bare `bo-ios` and malformed suffixes (`bo-ios-`, `bo-ios-abc`,
  `bo-ios-0`) stay blocked.
- Record iOS clients in the usage log with their own platform marker `I`,
  alongside the existing Android marker `A`; `-` stays "unknown".
- Keep the Android-only `Accept-Encoding` compatibility downgrade
  (`fix_bad_accept_header`) Android-only: an iOS client's `Accept-Encoding` is
  never stripped.
- No JSON-RPC parameter or response change, no CLI flag and no database schema
  change: only the accepted user agent and the derived usage-log platform /
  version values change. Database content is untouched.

## Capabilities

### New Capabilities

- `client-user-agent`: recognition of the first-party Blitzortung clients from
  the `User-Agent` request header — which clients the data endpoints allow and
  the platform/version recorded for them in the usage log.

### Modified Capabilities

<!-- None: there is no existing spec whose requirements change. -->

## Impact

- `src/service.rs` — user-agent parsing (`Request::user_agent_version` and the
  `USER_AGENT_PREFIX` constant), the forbidden-client check and
  `fix_bad_accept_header`.
- `src/service_log.rs` — `user_agent_version` / `client_platform` gain the iOS
  prefix and the `I` marker.
- `src/stats/parse.rs` — the platform-column documentation gains `I`; parsing
  already treats the column as an opaque string.
- `README.md`, `doc/jsonrpc_methods.md` — document the accepted client user
  agents (`bo-android-<int>`, `bo-ios-<int>`) and the `I` platform marker.
- Tests: `src/service.rs`, `src/service_log.rs`, `src/stats`, `tests/http_server.rs`
  and `tests/access_logging.rs` gain iOS cases.
- Out-of-scope dependency: `wuan/bo-ios` must send `bo-ios-<int>` before the app
  benefits; until it does, the app's bare `bo-ios` requests stay blocked.
