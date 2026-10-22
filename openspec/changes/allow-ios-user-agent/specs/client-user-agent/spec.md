# Spec Delta

## Purpose

Recognises the first-party Blitzortung mobile clients from their `User-Agent`
header so the data endpoints accept them and the usage log records a platform
and version for each.

## ADDED Requirements

### Requirement: iOS client user agents are accepted

The system SHALL accept data requests from the Blitzortung iOS client in
addition to the existing Android client. A `User-Agent` whose first
whitespace-separated word is `bo-ios` SHALL be accepted without a version
suffix, and one that is `bo-ios-<integer>` SHALL be accepted as a versioned iOS
client. An Android client SHALL still be accepted only as
`bo-android-<integer>`. Any other user agent SHALL remain blocked.

#### Scenario: Bare iOS user agent is allowed

- **WHEN** a data request's `User-Agent` is `bo-ios`
- **THEN** the request passes the user-agent check and is served rather than blocked

#### Scenario: Versioned iOS user agent is allowed

- **WHEN** a data request's `User-Agent` is `bo-ios-42`
- **THEN** the request passes the user-agent check

#### Scenario: Existing Android clients keep working

- **WHEN** a data request's `User-Agent` is `bo-android-190`
- **THEN** the request passes the user-agent check

#### Scenario: Unknown user agents stay blocked

- **WHEN** a data request's `User-Agent` is `Mozilla/5.0` or the header is missing
- **THEN** the request is blocked and returns `{}`

#### Scenario: Malformed iOS user agents stay blocked

- **WHEN** a data request's `User-Agent` is `bo-ios-abc` or `bo-ios-`
- **THEN** the request is blocked

### Requirement: Usage log records the client platform and version

The usage log SHALL record `A` in the platform column for an Android client and
`I` for an iOS client, and `-` when the client is unknown. The version column
SHALL hold the integer version of a versioned `bo-android-<n>` or `bo-ios-<n>`
client and SHALL be absent for a client whose user agent carries no version,
including bare `bo-ios`.

#### Scenario: Android client is logged as A with its version

- **WHEN** a `bo-android-190` request is written to the usage log
- **THEN** the platform column is `A` and the version column is `190`

#### Scenario: Bare iOS client is logged as I without a version

- **WHEN** a `bo-ios` request is written to the usage log
- **THEN** the platform column is `I` and the version column is absent

#### Scenario: Versioned iOS client is logged as I with its version

- **WHEN** a `bo-ios-42` request is written to the usage log
- **THEN** the platform column is `I` and the version column is `42`

#### Scenario: Unknown client is logged as unknown

- **WHEN** a request with no recognised client user agent is written to the usage log
- **THEN** the platform column is `-` and the version column is absent

### Requirement: Gzip compatibility downgrade stays Android-only

The system SHALL strip the `Accept-Encoding` request header only for Android
clients whose version is at or below the compatible maximum, and SHALL NOT strip
it for an iOS client or any other client.

#### Scenario: Old Android client is downgraded

- **WHEN** a `bo-android-177` request advertises `Accept-Encoding: gzip`
- **THEN** the header is removed and the response is not gzip-compressed

#### Scenario: iOS client keeps its Accept-Encoding

- **WHEN** a `bo-ios` request advertises `Accept-Encoding: gzip`
- **THEN** the header is kept and the response may be gzip-compressed
