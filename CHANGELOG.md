# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-06-22

First public release. A lightweight standalone mock HTTP server driven by a
declarative YAML configuration, designed for local development, integration
tests and CI/CD.

### Added

- **CLI** (`mockd` binary)
  - `mockd serve <config.yaml>` — start the server
  - `mockd validate <config.yaml>` — parse and compile the config without serving
  - `serve --cors` — enable permissive CORS support (preflight handling +
    `Access-Control-Allow-Origin: *` on every response)

- **Routing & matching**
  - HTTP methods: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`
  - Path patterns with parameters, e.g. `/users/{id}`
  - Request matching (`when:`) on:
    - query parameters (exact match)
    - headers (case-insensitive)
    - JSON body (subset match — every field must be present and equal)
  - First-match-wins ordering of routes

- **Responses**
  - Custom HTTP status codes and headers
  - JSON response bodies
  - Artificial delays (`delay: 2s`, `250ms`, `1m 30s`) for timeout testing
  - `close_connection: true` to send `Connection: close` and close the socket
  - **Sequence responses** (`response.sequence:`): return each item in order on
    successive calls; the last item is sticky. Enables retry, polling and
    pagination tests.

- **Templating**
  - `{{path.<name>}}` — captured path parameter
  - `{{query.<name>}}` — query parameter
  - `{{header.<name>}}` — request header (case-insensitive name)
  - `{{body.<json.path>}}` — dot-navigate the parsed JSON request body
    (object fields and array indices)
  - Whole-string expressions are coerced to the best-fitting JSON type
    (`42` → number, `true` → bool, `null` → null, otherwise string); partial
    interpolation is rendered as text.

- **Helper functions in templates**
  - `{{uuid}}` — fresh UUIDv4 string
  - `{{now}}` — current UTC time as ISO 8601 (`YYYY-MM-DDTHH:MM:SSZ`)
  - `{{randomInt(min,max)}}` — random integer in the inclusive `[min, max]` range
  - `{{random}}` — random 64-bit integer

- **Observability**
  - Per-request structured logging via `tracing` (one line per request with
    `method`, `path`, `status`)
  - Tunable via the standard `RUST_LOG` environment variable (defaults to
    `mockd=info`)

- **Project infrastructure**
  - Dual-licensed under MIT OR Apache-2.0
  - CI workflows for lint (rustfmt + clippy), tests, release builds, and
    security checks (`cargo-deny`, `cargo-audit`)
  - Release workflow that publishes pre-built binaries for
    Linux (amd64/arm64), macOS (amd64/arm64) and Windows (amd64), with
    SHA256 checksums, plus automatic publication to crates.io

### Known limitations

- Query parameter values are not percent-decoded.
- Request bodies are parsed as JSON; non-JSON bodies disable `body:` matching
  and `{{body.*}}` template lookups (both resolve to `null`).
- Response headers do not support multiple values for the same name
  (e.g. multiple `Set-Cookie` headers).
- HTTPS is not supported; terminate TLS at a reverse proxy if needed.

[Unreleased]: https://github.com/denislituev/mockd/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/denislituev/mockd/releases/tag/v0.1.0
