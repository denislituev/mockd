# Usage guide

This document complements the `README.md` with worked examples for the most
common mockd patterns.

## Running mockd

```bash
mockd serve mocks.yaml
mockd validate mocks.yaml
```

- `serve` binds the configured `listen` address and serves until interrupted
  (Ctrl-C).
- `validate` parses and compiles the config without starting the server. Use it
  in CI to fail fast on broken configs.

## Pattern: simple JSON endpoint

```yaml
routes:
  - method: GET
    path: /health
    response:
      status: 200
      body:
        ok: true
```

## Pattern: path parameter + templating

```yaml
routes:
  - method: GET
    path: /users/{id}
    response:
      status: 200
      body:
        id: "{{path.id}}"
        name: "User {{path.id}}"
```

`GET /users/42` returns `{"id":42,"name":"User 42"}`. The whole-string
expression `{{path.id}}` is coerced to a JSON number.

## Pattern: query-based conditional response

Declare the more specific route first; mockd falls through to the next route
when the `when` block does not match.

```yaml
routes:
  - method: GET
    path: /users
    when:
      query:
        role: admin
    response:
      status: 200
      body:
        admin: true

  - method: GET
    path: /users
    response:
      status: 200
      body:
        admin: false
```

## Pattern: header matching (case-insensitive)

```yaml
routes:
  - method: GET
    path: /tenants/me
    when:
      headers:
        X-Tenant-Id: tenant-a
    response:
      status: 200
      body:
        tenant: "{{header.X-Tenant-Id}}"
```

Header names are matched case-insensitively, so `x-tenant-id` and
`X-Tenant-Id` are equivalent.

## Pattern: request body subset matching

```yaml
routes:
  - method: POST
    path: /login
    when:
      body:
        username: admin
    response:
      status: 200
      body:
        token: admin-token

  - method: POST
    path: /login
    response:
      status: 401
      body:
        error: invalid credentials
```

The `body` matcher is a **subset** match: extra fields in the request are
ignored, but every field you list must be present and equal.

## Pattern: simulating failures and slow responses

```yaml
routes:
  - method: GET
    path: /fail
    response:
      status: 500
      body:
        error: something went wrong

  - method: GET
    path: /slow
    response:
      status: 200
      delay: 2s
      body:
        ok: true

  - method: GET
    path: /drop
    response:
      status: 200
      close_connection: true
      body:
        dropped: true
```

- `delay` accepts human durations: `500ms`, `2s`, `1m 30s`.
- `close_connection` sends `Connection: close` so the client reconnects on the
  next request.

## Templating cheat sheet

| Expression                  | Source         | Notes                              |
| --------------------------- | -------------- | ---------------------------------- |
| `{{path.id}}`               | path param     | coerced when whole-string          |
| `{{query.role}}`            | query param    | string                             |
| `{{header.X-Tenant-Id}}`    | request header | matched case-insensitively         |
| `"prefix-{{path.id}}"`      | interpolation  | always string                      |

## Limitations of the MVP

- Query values are not percent-decoded.
- Body templating (echoing request body fields in the response) is not
  supported — only `path`/`query`/`header` sources.
- Only JSON bodies are matched; non-JSON request bodies are treated as empty.
- `close_connection` signals intent via the `Connection: close` header rather
  than forcibly terminating the TCP stream.

These are deliberate MVP boundaries; see the roadmap in `README.md`.
