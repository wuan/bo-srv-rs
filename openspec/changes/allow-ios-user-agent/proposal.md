# Proposal

## Why

The Blitzortung iOS app identifies itself with the user agent `bo-ios`, but the
service recognises only `bo-android-<int>`. `is_forbidden` therefore rejects
every iOS request with `{}`, so the first-party iOS client cannot use the
service at all. Android and iOS are both Blitzortung clients and should be
accepted, logged and counted the same way.

## What Changes

- Accept requests whose `User-Agent` identifies the Blitzortung iOS client —
  bare `bo-ios` (the form the iOS app actually sends) and a versioned
  `bo-ios-<int>` form — in addition to the existing `bo-android-<int>` client.
  No version suffix is required for the iOS client.
- Record iOS clients in the usage log with their own platform marker `I` and
  their parsed version when the user agent carries one. `A` stays the Android
  marker and `-` stays "unknown"; the version column stays absent for a client
  without a version.
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
  agents and the `I` platform marker.
- Tests: `src/service.rs`, `src/service_log.rs`, `src/stats`, `tests/http_server.rs`
  and `tests/access_logging.rs` gain iOS cases.
