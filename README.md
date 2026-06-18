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

## Features (MVP)

- HTTP methods: `GET`, `POST`, `PUT`, `PATCH`, `DELETE`
- Path parameters: `/users/{id}`
- Request matching on query parameters, headers (case-insensitive) and JSON body
  (subset match)
- Response bodies as JSON, with templating:
  - `{{path.id}}`
  - `{{query.role}}`
  - `{{header.X-Tenant-Id}}`
- Custom status codes and headers
- Artificial delays (`delay: 2s`) for timeout testing
- `close_connection: true` to drop the connection after responding

## Installation

```bash
cargo install --path .
```

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

| field              | type       | default | description                                            |
| ------------------ | ---------- | ------- | ----------------------------------------------------- |
| `status`           | u16        | `200`   | HTTP status code                                       |
| `headers`          | map        | `{}`    | Response headers                                       |
| `body`             | any/json   | —       | JSON body, may contain template expressions            |
| `delay`            | duration   | —       | e.g. `2s`, `250ms`, `1m 30s`                           |
| `close_connection` | bool       | `false` | Send `Connection: close` and close after responding    |

### Templating

Template expressions go inside string values in `body`:

```yaml
body:
  id: "{{path.id}}"
  role: "{{query.role}}"
  tenant: "{{header.X-Tenant-Id}}"
  label: "user-{{path.id}}"
```

- When the **whole** string value is a single expression, the result is coerced
  to the best-fitting JSON type (`42` → number, `true` → bool, `null` → null,
  otherwise string). This is how `id: "{{path.id}}"` becomes `42` rather than
  `"42"`.
- When the expression is part of a larger string, it is interpolated as text.
- Missing/unknown variables resolve to JSON `null` (whole-string) or an empty
  string (interpolation).

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

## Roadmap (post-MVP)

The current architecture is intentionally minimal but extensible. Planned:

- Sequence responses (`response.sequence:`)
- Stateful responses (`state:`)
- OpenAPI import (`mockd import openapi.yaml`)
- Request recording / replay

The following are explicitly **out of scope** for now: GUI/Web UI, Kubernetes
operator, gRPC, GraphQL, state machines.

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.
