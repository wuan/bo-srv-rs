# Tasks

## 1. Accept the iOS client in the request path

- [ ] 1.1 Add a user-agent client parser to `src/service.rs` that returns a
      recognised client kind plus an optional version (`bo-android-<n>`,
      `bo-ios`, `bo-ios-<n>`, else unknown), and verify with unit tests that
      `bo-ios` and `bo-ios-42` parse, and that `bo-android`, `bo-android-`,
      `bo-android-abc`, `bo-android-0`, `bo-ios-abc`, `bo-ios-`, `Mozilla/5.0`
      and a missing header do not.
- [ ] 1.2 Use the parser in `forbidden_client_reason` so a recognised client —
      including bare `bo-ios` — is not blocked, and verify with unit tests that
      a `bo-ios` request is allowed while an unknown or missing user agent is
      still forbidden.
- [ ] 1.3 Keep `fix_bad_accept_header` Android-only and verify with unit tests
      that `bo-android-177` still has `Accept-Encoding` stripped while `bo-ios`
      (and `bo-ios-177`) keeps it.
- [ ] 1.4 Update the `is_forbidden` / compression wording in `README.md` and
      `doc/jsonrpc_methods.md` to name `bo-ios` alongside `bo-android-<int>`,
      and check the documented examples against the new tests.

## 2. Record the platform and version in the usage log

- [ ] 2.1 Extend `user_agent_version` and `client_platform` in
      `src/service_log.rs` so `bo-ios` / `bo-ios-<n>` yield platform `I` and the
      parsed version when present, and verify with unit tests covering
      `bo-android-190` → `A`/`190`, `bo-ios` → `I`/absent, `bo-ios-42` →
      `I`/`42` and an unknown user agent → `-`/absent.
- [ ] 2.2 Update the usage-log platform documentation in `README.md`, the field
      comments in `src/service_log.rs` and the platform comment in
      `src/stats/parse.rs` to describe `A`, `I` and `-`, and add a stats test
      that a row with platform `I` parses.

## 3. Integration verification

- [ ] 3.1 Add a test in `tests/http_server.rs` or `tests/access_logging.rs`
      that a request with `User-Agent: bo-ios` is served (a real result, not the
      blocked `{}`) and, where the usage log is captured, records platform `I`;
      run that test file.
- [ ] 3.2 Run `cargo test` and `cargo clippy` and confirm the iOS and existing
      Android cases pass together.

## Workflow follow-up

- Archive the change after the project's review requirements are satisfied.
- Verify the archived spec and updated main spec after archive.
