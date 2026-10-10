# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Multiple response headers** — a response header value can now be a list,
  emitting the header once per value (primarily for `Set-Cookie`):

  ```yaml
  response:
    headers:
      Set-Cookie:
        - "session=abc; Path=/"
        - "theme=dark; Path=/"
  ```

  A plain string keeps setting a single header as before.
  *Breaking for library users:* `ResponseConfig.headers` is now
  `HashMap<String, HeaderValues>` instead of `HashMap<String, String>`;
  the JSON Schema was regenerated accordingly.

### Changed

- **Percent-decoding of query values and path parameters** —
  `?name=John%20Doe` now matches `when.query.name: "John Doe"` and renders
  `{{query.name}}` / `{{path.id}}` with the decoded value. Only `%XX`
  sequences are decoded: `+` stays a literal plus sign, and an encoded
  slash keeps its segment intact (`/files/a%2Fb%20c` still matches
  `/files/{name}` with the value `a%2Fb c` — only the `%2F` escape itself
  stays encoded).

## [0.4.0] - 2026-10-09

### Added

- **`OPTIONS` as a routable method** — routes with `method: OPTIONS` now
  receive non-preflight `OPTIONS` requests. CORS preflights (`OPTIONS` with
  `Access-Control-Request-Method`) are still intercepted by `--cors` before
  route matching.
- **`HEAD` support** — `HEAD` requests are answered by `GET` routes with the
  same status and headers (including `Content-Length`) but no body,
  per RFC 9110.
- **Shadowed-route warnings in `mockd validate`** — warns when an earlier
  route with the same method and a same-or-more-general path pattern (and
  no `when` conditions) makes a later route unreachable due to
  first-match-wins, e.g. `route 1 (/users) is unreachable: it is shadowed
  by earlier route 0 (/users)`. The check is conservative: it only reports
  clear cases and never fails validation.

### Changed

- **Parse errors now include the exact config path** — invalid values are
  reported as `routes[0].method: unknown variant ...`,
  `routes[2].when.query.email: invalid matcher: ...` instead of a generic
  `data did not match any variant of untagged enum`. Messages for invalid
  matchers and responses spell out the expected shape. Powered by
  `serde_path_to_error`.
  *Breaking for library users:* `ConfigError::Parse` is now a struct variant
  `Parse { message, source }` instead of a tuple `Parse(serde_yaml::Error)`;
  the underlying error is available via `source()` instead of destructuring.

## [0.3.0] - 2026-10-08

### Added

- **Matcher operators in `when:`** — query and header values now support three
  forms: a plain string (exact match), `matches:` (regular expression) and
  `contains:` (substring):

  ```yaml
  when:
    query:
      email:
        matches: "^[^@]+@example\\.com$"
    headers:
      X-Environment:
        contains: staging
  ```

  Regexes are compiled once when the configuration is loaded; an invalid
  regex is reported with the exact config path
  (`invalid regex in route 0 at when.query.email`). `matches` is a search,
  not a full match — use `^...$` anchors to match the whole value. Header
  names and exact header values remain case-insensitive; `matches` and
  `contains` are case-sensitive (`(?i)` for case-insensitive regexes).
  A matcher object must have exactly one key: combining `matches` with
  `contains`, or adding unknown keys, is a config error.

- **Matcher operators inside JSON body patterns** — an object with exactly
  one key `matches`/`contains` and a string value is an operator applied to
  the corresponding string value of the request body; objects with more
  keys (or non-string values) keep the literal subset semantics:

  ```yaml
  when:
    body:
      role: admin
      email:
        matches: ".*@example\\.com$"
  ```

  Operators only apply to string values in the request body; a non-string
  value never matches an operator.

- **Route matching diagnostics** — with `RUST_LOG=mockd=debug` mockd logs
  why each candidate route was skipped (`method mismatch`, `path mismatch`,
  or the exact `when` condition that failed, e.g.
  `query "role": expected "admin"`). The default `info` output stays one
  line per request. Diagnostics never include actual request values for
  query parameters and body fields (type mismatches report the JSON type);
  sensitive headers (`Authorization`, `Proxy-Authorization`, `Cookie`,
  `Set-Cookie`) are reported without their values.

## [0.2.0] - 2026-07-01

### Added

- **`mockd generate` CLI command** — generates a JSON Schema for the
  configuration file from the Rust types via `schemars`. The schema is
  published to GitHub Pages at
  `https://denislituev.github.io/mockd/schema.json` and can be referenced
  from `mocks.yaml` via a `# yaml-language-server: $schema=...` hint for
  editor autocompletion, hover-docs and inline validation.
- **Graceful shutdown** — `mockd serve` now handles `SIGINT` (Ctrl+C) and
  `SIGTERM` by stopping the server cleanly instead of dropping active
  connections. Important for CI runners that send `SIGTERM` on timeout.

### Changed

- The package name on crates.io is now `mockd-http` (the `mockd` name was
  already taken). The binary is still installed as `mockd`.

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

- **Editor support**
  - A [JSON Schema](https://denislituev.github.io/mockd/schema.json) for the
    YAML configuration, generated from the Rust types via `schemars`. Add a
    `# yaml-language-server: $schema=...` hint to your `mocks.yaml` to get
    autocompletion, hover-docs and inline validation in VS Code, Zed,
    IntelliJ and other editors. A unit test guards against drift between
    the schema and the code.

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

<!-- links -->

[Unreleased]: https://github.com/denislituev/mockd/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/denislituev/mockd/releases/tag/v0.4.0
[0.3.0]: https://github.com/denislituev/mockd/releases/tag/v0.3.0
[0.2.0]: https://github.com/denislituev/mockd/releases/tag/v0.2.0
[0.1.0]: https://github.com/denislituev/mockd/releases/tag/v0.1.0
