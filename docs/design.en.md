# Design notes

**English** · [简体中文](design.md) · [Back to README](../README.en.md)

## Extraction flow

`Client::parse` extracts a URL from the input, identifies the platform, passes
the rate-limit gate and calls the platform extractor. Results share the
`VideoInfo` model, with source, page URL and required media headers filled in
and formats normalized. See [lib.rs](../crates/alcedo/src/lib.rs) for the public entry point.

Platform detection checks the URL hostname and domain boundaries, so a platform
name in a query parameter does not identify the destination. A `match` dispatches
known platforms to their extraction functions. New platforms must update
[registry.rs](../crates/alcedo/src/registry.rs) and [parsers/mod.rs](../crates/alcedo/src/parsers/mod.rs).

## HTTP and request controls

The [HTTP layer](../crates/alcedo/src/http/mod.rs) shares clients per proxy
configuration within the process, reusing connections, TLS sessions and DNS
caches. It follows redirects explicitly and checks each destination. A custom
[DNS resolver](../crates/alcedo/src/http/ssrf.rs) rejects private addresses in
DNS results by default.

[Rate limits and circuit breakers](../crates/alcedo/src/http/resilience.rs) keep
shared state per platform. Five consecutive qualifying failures pause requests,
with a cooldown starting at 15 seconds and increasing exponentially to five
minutes. `blocked`, `network`, `timeout` and `login` count toward the breaker;
errors such as deleted content do not.

Some extraction paths use a [guest identity cache](../crates/alcedo/src/http/identity.rs).
These mechanisms reduce repeated requests and handle temporary failures; they
do not guarantee that a platform will accept requests.

## Page data and media formats

[json_after](../crates/alcedo/src/util.rs) scans matching braces after a marker
while handling JSON strings and escapes, so subsequent JavaScript statements
are not included in the JSON. Platform extractors convert the result into the
shared model.

The [YouTube extractor](../crates/alcedo/src/parsers/youtube.rs) tries multiple
clients, uses directly available media URLs and skips entries with
`signatureCipher`. Extraction can still fail when no usable URL is returned.
The [Bilibili extractor](../crates/alcedo/src/parsers/bilibili.rs) requests DASH
data and lists available audio and video tracks. Formats depend on content and
access permissions; callers handle downloading and merging.

Douyin's optional signing service is separate from the extractor so its
implementation can be updated independently. See [external signers](signers.en.md).

## Performance measurement

Repeat extraction for one link within a single process. Replace `<url>` with an
accessible share link:

```bash
cargo run --release -p alcedo --example bench -- "<url>" 10
```

The [measurement program](../crates/alcedo/examples/bench.rs) prints each request's
elapsed time, success count, minimum and middle sorted sample. The summary
includes the first successful extraction and selects the upper middle sample
for an even count. Failed requests are excluded, so the result is not a
separate measurement of warm connections.

When sharing results, record the commit, date, hardware, test link, proxy or
sign-in conditions and success ratio. To compare initial and subsequent
requests, calculate them separately from the per-request output. End-to-end
timings include the network and platform response time.
