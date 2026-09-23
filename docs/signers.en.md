# External signers

**English** · [简体中文](signers.md) · [Back to README](../README.en.md)

A signer adds query parameters, headers and cookies to platform requests. The
Douyin extractor currently uses this mechanism. An external program or HTTP
service supplies the signing algorithm; this repository does not ship one.

## Configuration

For a local program, use the subprocess transport and replace the executable path:

```bash
export ALCEDO_SIGNER_DOUYIN="cmd:/opt/alcedo/signer"
```

Alternatively, point to an HTTP service, replacing the example endpoint:

```bash
export ALCEDO_SIGNER_DOUYIN="https://signer.example.com/sign"
```

In PowerShell, use `$env:ALCEDO_SIGNER_DOUYIN = "..."`. The signer reads this
setting directly from the environment; it is not part of `Config`.

HTTP services use the extractor's HTTP layer, proxy settings and address checks.
Loopback and private IP endpoints such as `127.0.0.1` are currently rejected.
Use the subprocess transport for local signers.

## Transport and protocol

- Each subprocess invocation receives one JSON object on stdin, followed by EOF.
  It must write only one response JSON object to stdout and exit with status 0.
  Commands are split on whitespace without a shell, so paths and arguments
  containing spaces are not supported.
- HTTP transport POSTs the request JSON to the endpoint and reads JSON from a
  successful response.

Both transports use the same fields. Example request:

```json
{
  "platform": "douyin",
  "url": "https://www.douyin.com/aweme/v1/web/aweme/detail/",
  "query": "aweme_ids=%5B123%5D",
  "user_agent": "Mozilla/5.0 ...",
  "body": ""
}
```

The extractor supplies `url`, `query` and `body`; compute signatures from the
received values. `user_agent` is the UA supplied by the caller.

Example response. All three fields are optional, with string keys and values:

```json
{
  "query": { "a_bogus": "..." },
  "headers": {},
  "cookies": { "msToken": "..." }
}
```

`query` is appended to the request URL, with values encoded by the caller.
`headers` and `cookies` are merged into the platform request. Return `{}` to
add no signature.

## Timeouts and failure handling

After input is written, the subprocess has a five-second limit to exit and
return its output. HTTP uses the normal request timeout
(`ALCEDO_REQUEST_TIMEOUT`, 20 seconds by default). The entire extraction is
also subject to `ALCEDO_TOTAL_TIMEOUT`.

An unset signer, invocation failure, nonzero exit status, unsuccessful HTTP
status or invalid response JSON produces an empty signature. Invocation
failures log a warning. Extraction continues without the signature, but the
platform can still reject the request, and the whole-parse timeout can stop
the operation.

See [signer.rs](../crates/alcedo/src/parsers/signer.rs) for implementation and protocol tests.
