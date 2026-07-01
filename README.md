# Mockd

**Mockd** is a lightweight standalone mock HTTP server for local development,
integration tests and CI/CD. You describe your API with a declarative YAML
config — no code required — and mockd serves it.

```bash
mockd serve mocks.yaml
```

It aims to be simpler than [WireMock] / [MockServer], but convenient for the
day-to-day work of microservice developers.

[WireMock]: https://wiremock.org/
[MockServer]: https://www.mock-server.com/

---

## Features

- HTTP methods: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`
- Path parameters: `/users/{id}`
- Request matching on query parameters, headers (case-insensitive) and JSON body
  (subset match)
- Response bodies as JSON, with templating:
  - `{{path.id}}`
  - `{{query.role}}`
  - `{{header.X-Tenant-Id}}`
  - `{{body.user.name}}` (dot navigation through the request body)
  - helper functions: `{{uuid}}`, `{{now}}`, `{{randomInt(min,max)}}`
- Custom status codes and headers
- Artificial delays (`delay: 2s`) for timeout testing
- `close_connection: true` to drop the connection after responding
- Sequence responses (`response.sequence:`) for testing retry/polling
- Permissive CORS via `--cors` (handles `OPTIONS` preflight, adds
  `Access-Control-Allow-Origin: *`)
- Request logging via `tracing` (one structured line per request)

## Installation

From source (any platform with a Rust toolchain):

```bash
cargo install --path .
```

Once published to crates.io (the package name is `mockd-http`; the binary
is still installed as `mockd`):

```bash
cargo install mockd-http
```

Pre-built binaries for Linux (amd64/arm64), macOS (amd64/arm64) and Windows
(amd64) are published on the [releases page][releases].

[releases]: https://github.com/denislituev/mockd/releases

## Quick start

Create `mocks.yaml`:

```yaml
listen: ":8080"

routes:
  - method: GET
    path: /users/{id}
    response:
      status: 200
      body:
        id: "{{path.id}}"
        name: "User {{path.id}}"
```

Start the server:

```bash
mockd serve mocks.yaml
```

Try it:

```bash
curl http://localhost:8080/users/42
# {"id":42,"name":"User 42"}
```

Validate a config without serving:

```bash
mockd validate mocks.yaml
```

For browser/SPA testing, enable CORS:

```bash
mockd serve mocks.yaml --cors
```

See [`examples/users.yaml`](examples/users.yaml) for a more complete example
covering matching, templates, delays and errors.

## Configuration reference

### Top level

| field    | type          | default  | description                          |
| -------- | ------------- | -------- | ------------------------------------ |
| `listen` | string        | `:8080`  | Socket address to bind               |
| `routes` | list[route]   | `[]`     | Routes, first match wins             |

### Route

| field     | type     | description                                   |
| --------- | -------- | --------------------------------------------- |
| `method`  | enum     | `GET`/`POST`/`PUT`/`PATCH`/`DELETE`           |
| `path`    | string   | Path pattern, e.g. `/users/{id}`              |
| `when`    | match?   | Optional request matcher (see below)          |
| `response`| response | Response produced when the route matches      |

### `when` (request matcher)

All conditions are optional; all present conditions must be satisfied.

```yaml
when:
  query:
    role: admin
  headers:
    X-Tenant-Id: tenant-a
  body:
    username: admin
```

- `query`: required query parameters (exact match).
- `headers`: required headers (matched case-insensitively).
- `body`: a JSON object that must be a **subset** of the request body. Every
  field you list must be present and equal; extra fields in the request are
  ignored. Arrays must match element-by-element with the same length.

### `response`

A route's `response` can be either a single response or a **sequence** of
responses (see [Sequence responses](#sequence-responses) below).

| field              | type       | default | description                                            |
| ------------------ | ---------- | ------- | ----------------------------------------------------- |
| `status`           | u16        | `200`   | HTTP status code                                       |
| `headers`          | map        | `{}`    | Response headers                                       |
| `body`             | any/json   | —       | JSON body, may contain template expressions            |
| `delay`            | duration   | —       | e.g. `2s`, `250ms`, `1m 30s`                           |
| `close_connection` | bool       | `false` | Send `Connection: close` and close after responding    |

### Sequence responses

Wrap multiple responses in `response.sequence:` to make mockd return each one
in order on successive calls. The last item is **sticky**: it is repeated on
every call once the previous items have been exhausted.

```yaml
- method: GET
  path: /flaky
  response:
    sequence:
      - status: 500
        body: { error: transient }
      - status: 500
        body: { error: transient }
      - status: 200
        body: { ok: true }
```

Useful for testing retry logic, polling, pagination and any client behavior
that depends on a sequence of states.

A single `response: {...}` is equivalent to a one-element sequence.

### Templating

Template expressions go inside string values in `body`:

```yaml
body:
  id: "{{path.id}}"
  role: "{{query.role}}"
  tenant: "{{header.X-Tenant-Id}}"
  label: "user-{{path.id}}"
  echoed_name: "{{body.user.name}}"
  request_id: "{{uuid}}"
  created_at: "{{now}}"
  priority: "{{randomInt(1,5)}}"
```

Lookups:

| Namespace | Example            | Meaning                                        |
| --------- | ----------------- | ---------------------------------------------- |
| `path`    | `{{path.id}}`      | A captured path parameter                      |
| `query`   | `{{query.role}}`   | A query parameter                              |
| `header`  | `{{header.X-Foo}}` | A request header (case-insensitive name)       |
| `body`    | `{{body.user.id}}` | Dot-navigate the parsed JSON request body;     |
|           |                   | numeric segments index into arrays             |

Helper functions (no namespace):

| Expression              | Returns                                          |
| ----------------------- | ------------------------------------------------ |
| `{{uuid}}`              | A fresh UUIDv4 string                            |
| `{{now}}`               | Current UTC time as ISO 8601 (`...Z`)            |
| `{{randomInt(min,max)}}`| Random integer in the inclusive `[min, max]` range |
| `{{random}}`            | A random 64-bit integer (whole range)            |

Coercion rules:

- When the **whole** string value is a single expression, the result is coerced
  to the best-fitting JSON type (`42` → number, `true` → bool, `null` → null,
  otherwise string). This is how `id: "{{path.id}}"` becomes `42` rather than
  `"42"`.
- When the expression is part of a larger string, it is interpolated as text.
- Missing/unknown variables resolve to JSON `null` (whole-string) or an empty
  string (interpolation). Helper functions always resolve to a value.

### CORS

Pass `--cors` to enable permissive cross-origin support:

- Every response gets `Access-Control-Allow-Origin: *`.
- `OPTIONS` preflight requests (i.e. they carry `Access-Control-Request-Method`)
  are answered with `204 No Content` **without** consulting the routes. The
  `Access-Control-Allow-Headers` value is echoed from the request.

This is intended for local development where the mock server and the frontend
run on different origins (e.g. `localhost:8080` mock + `localhost:3000` Vite).

### Logging

mockd logs every request to stderr via `tracing`. The default level is `info`
(one structured line per request). Tune with the standard `RUST_LOG`
environment variable:

```bash
RUST_LOG=mockd=warn mockd serve mocks.yaml     # quieter
RUST_LOG=mockd=debug mockd serve mocks.yaml    # verbose (includes delays)
```

### Editor support (JSON Schema)

A [JSON Schema](https://json-schema.org/) for the configuration file is
published at:

```
https://denislituev.github.io/mockd/schema.json
```

Add this comment to the top of your `mocks.yaml` to get autocompletion,
hover-documentation and inline validation in editors that support the
`yaml-language-server` convention (VS Code with the Red Hat YAML extension,
Zed, IntelliJ, Neovim):

```yaml
# yaml-language-server: $schema=https://denislituev.github.io/mockd/schema.json
listen: ":8080"
routes:
  # ...
```

The schema is generated from the Rust types in [`src/config.rs`](src/config.rs)
via [`schemars`](https://crates.io/crates/schemars). Regenerate it with
`mockd generate`; a unit test guards against drift between the committed
schema and the code.

## Architecture

Mockd is a single crate split into focused modules:

```
src/
├── main.rs       # CLI (clap): `serve` and `validate`
├── lib.rs        # library root
├── config.rs     # domain models + YAML loading
├── router.rs     # request matching (server-agnostic)
├── template.rs   # {{...}} rendering
└── server.rs     # Axum HTTP layer
```

The `router` is deliberately independent of the HTTP server, which makes it
cheap to unit-test and reuse.

## Running the tests

```bash
cargo test
```

Unit tests live next to the code (`#[cfg(test)]` modules); end-to-end tests are
in [`tests/integration.rs`](tests/integration.rs) and spin up a real server on
an ephemeral port.

## Roadmap

The following are planned for future releases, in rough priority order:

- Stateful responses (`state:`)
- OpenAPI import (`mockd import openapi.yaml`)
- Request recording / replay

The following are explicitly **out of scope** for now: GUI/Web UI, Kubernetes
operator, gRPC, GraphQL, state machines.

See [CHANGELOG.md](CHANGELOG.md) for what is included in this release.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
