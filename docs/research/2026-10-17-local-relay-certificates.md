# Research memo: certificates for the local moq relays

2026-10-17, for the moq-relays change. Read-only research against primary sources; no code was
run. Sources read from `/tmp`: `moq-relay 0.17.6`, `moq-tokio 0.19.27`, `rcgen 0.14.10`,
`@moq/net 0.4.3`. Tags: **[V]** verified (spec, source line or official doc read), **[S]**
secondary, **[I]** inferred from [V] facts, **[U]** unverified.

## Question

`tatolabd` runs two `moq-relay` children per machine (loopback 127.0.0.1:4443, every-interface
[::]:4445) and provisions each one's certificate and key as files the relay hot-reloads; the
preview page opens `http://host:port` and `@moq/net` pins `/certificate.sha256` through
`serverCertificateHashes`. What do browsers require of that certificate, how does the relay load
and reload it, what can `tatolabd` issue programmatically, and what does a browser off the
machine need?

## Short answer

- **The pinned-certificate path is fully specified and interoperable: ECDSA P-256, X.509v3,
  validity ≤ 14 days, current time inside it, SHA-256 over the leaf DER, no CA and no hostname
  match.** Chromium, Firefox (125+) and WebKit (landed 2025-10) each enforce the 14-day cap, and
  Chromium and Firefox refuse RSA. A certificate that outlives the cap, or expires, fails the
  handshake — there is no interstitial. A publicly trusted certificate removes the hash entirely:
  with empty `serverCertificateHashes` the browser runs ordinary Web-PKI verification.
- **The relay side is ready for `tatolabd`-provisioned files.** `listen.tls.cert` / `listen.tls.key`
  are PEM paths, watched by directory with `notify`; a create, modify, rename or close-after-write
  reloads every configured pair and recomputes the fingerprints served at `/certificate.sha256`
  (first cert in config order, hex SHA-256 of the leaf DER). `listen.tls.generate` mints a cert once
  per process (ECDSA P-256, backdated one day, 14 days total) and never re-mints on reload, so a
  relay left running under `generate` for 13+ days stops being pinnable — one more reason the
  files are `tatolabd`'s.
- **The `http://` fingerprint trick does not travel.** `WebTransport` is `[SecureContext]`; an
  `http://` page on a LAN IP or plain hostname is not a secure context, so the constructor is
  absent off the machine. Off-machine pinning needs the page itself in a secure context (an
  `https://` page served under the same self-signed cert, after the user clicks through, or a
  Chrome flag), which is not a shareable URL.
- **Programmatic publicly trusted certificates exist for LAN hosts only through DNS-01**, which
  needs a public DNS name per machine and a provider credential (or an intermediary that does the
  TXT record for you, Tailscale-style). HTTP-01 / TLS-ALPN-01 and Let's Encrypt's IP certificates
  need the host reachable from the public internet on 80/443, which a LAN machine is not.
  `instant-acme` (0.8.5, 2026-02) is the maintained async Rust client with DNS-01, profiles and ARI.
- **Decision space:** (A) short-lived self-issued certs + fingerprint, on-machine only; (B) a public
  DNS name per machine with DNS-01 through a provider token held locally; (C) a Tatolab-operated
  zone that performs DNS-01 on the machine's behalf, keys staying local. B and C are the only
  shapes that give a plain shareable `https://` URL. What the owner must decide is listed at the end.

## Evidence

### 1. What the browsers require of a hash-pinned certificate

- [V] W3C WebTransport, §6.9 Configuration, `serverCertificateHashes`: "the user agent SHALL deem a
  server certificate trusted if and only if it can successfully verify a certificate hash against
  serverCertificateHashes"; when empty, "the user agent SHALL use certificate verification
  procedures it would use for normal fetch operations"; "This option is only supported for
  transports using dedicated connections", and "cannot be used with allowPooling".
  https://w3c.github.io/webtransport/#dom-webtransportoptions-servercertificatehashes
- [V] Same section, custom certificate requirements: "the certificate MUST be an X.509v3
  certificate as defined in [RFC5280]", "the current time MUST be within the validity period of
  the certificate", "the total length of the validity period MUST NOT exceed two weeks", and "The
  user agent MAY impose additional implementation-defined requirements". Allowed public key
  algorithms: "it MUST include ECDSA with the secp256r1 (NIST P-256) named group" and "It MUST NOT
  contain RSA keys". "Compute a certificate hash" is "the SHA-256 hash of cert" (the DER leaf);
  "verify a certificate hash" takes "the first certificate in certificate chain (the leaf
  certificate)" and compares against entries whose algorithm is `sha-256`, case-insensitive.
- [V] §6.2 constructor: "If url's scheme is not https, throw a SyntaxError"; the IDL is
  `[Exposed=(Window,Worker), SecureContext] interface WebTransport`. §14.1: "certificate
  verification errors are fatal" with no interstitial. §14.4: the mechanism "substitutes Web
  PKI-based authentication for an individual connection" and identifies the server "by a specific
  server certificate, rather than its server name".
- [V] Chromium `net/quic/dedicated_web_transport_http3_client.cc`: `constexpr int
  kCustomCertificateMaxValidityDays = 14;` passed to `ChromiumWebTransportFingerprintProofVerifier`,
  whose only override is `IsKeyTypeAllowedByPolicy` returning `false` for `PublicKeyType::kRsa`.
  The QUICHE base (`quiche/quic/core/crypto/web_transport_fingerprint_proof_verifier.cc`) checks
  `duration_seconds <= max_validity_` (+1 s slack), `now` strictly inside `[validity_start,
  validity_end]`, allows P-256 / P-384 / Ed25519, hashes `RawSha256(der_certificate)` of
  `certs[0]`, and ignores the hostname (`const std::string& /*hostname*/`).
  https://raw.githubusercontent.com/chromium/chromium/main/net/quic/dedicated_web_transport_http3_client.cc
- [V] Firefox `netwerk/protocol/http/WebTransportCertificateVerifier.cpp`,
  `AuthCertificateWithServerCertificateHashes`: `if (certDuration > Duration(60 * 60 * 24 * 14))
  return ERROR_VALIDITY_TOO_LONG`, `CheckValidity(now, notBefore, notAfter)`, RSA refused in
  `CheckRSAPublicKeyModulusSizeInBits` ("RSA is not supported for serverCertificateHashes"), any
  ECDSA curve accepted, `Digest::DigestBuf(SEC_OID_SHA256, peerCert, …)` compared to each hash whose
  algorithm is `"sha-256"`. Bugzilla 1873263 "RESOLVED FIXED", target "125 Branch".
- [V] WebKit bug 300057 "Implement WebTransportOptions.serverCertificateHashes" RESOLVED FIXED,
  committed 2025-10-02 (PR #51706: `constexpr double twoWeeks = 2 * 7 * 24 * 60 * 60`, `CC_SHA256`,
  exact-match `"sha-256"`). [S] caniuse lists Safari 26.4+.
- [I] Past 14 days: a certificate whose total validity exceeds the cap is refused from the
  start; one inside the cap is refused once `now` passes `notAfter`. Both are handshake failures
  the page sees as a rejected `ready`.

### 2. How moq-relay 0.17.6 / moq-tokio 0.19.27 load and reload certificates

- [V] `moq-relay` enables `moq-tokio` features `aws-lc-rs`, `watch`, `tcp`
  (`moq-relay-0.17.6/Cargo.toml:198-205`); `watch = ["dep:notify"]` (`moq-tokio/Cargo.toml:102`).
- [V] Config keys (`moq-tokio/src/tls.rs:1172-1247`, struct `Listen`): `listen.tls.cert` and
  `listen.tls.key` (PEM paths, "In config files, each list field accepts either a single string or
  a TOML array", paired by index), `listen.tls.generate` (hostnames; "won't be valid unless the
  client uses the fingerprint or disables verification"), `listen.tls.root` (mTLS client roots).
  CLI `--listen-tls-cert/-key/-generate`, env `MOQ_LISTEN_TLS_*`; the released `--tls-cert` /
  `MOQ_SERVER_TLS_*` spellings are refused by `refuse_deprecated` (`tls.rs:1363-1369`). The
  `[server]` table was renamed `[listen]` (`moq-relay/src/config.rs:311-316`).
- [V] Loading (`tls.rs:2789-2857`, `ServeCerts::load_certs`): cert/key counts must match; at least
  one of files, `generate` or an in-memory identity; each file pair goes through `read_certs` +
  `PrivateKeyDer::from_pem_file` and `keys_match()`; the generated cert is appended after the
  file-backed ones. `set_certs` (`:2861-2873`) replaces the list wholesale and recomputes
  `hex::encode(sha256(leaf DER))` per cert.
- [V] Hot reload (`tls.rs:2908-2998`, `src/watch.rs`): `Reload::spawn` watches only the
  file-backed paths (`generate`-only listeners get an inert task); `watch::Files` watches each
  file's **parent directory**, `NonRecursive`, because "cert-manager, and K8s secret mounts replace
  files by atomic rename or symlink swap". `is_reload_trigger` fires on
  `Access(Close(Write))`, `Create(_)`, `Modify(_)`; ignores reads and bare removals. The loop calls
  `load_certs` on every wake and keeps the old set on error ("failed to reload server
  certificates"). Reload is for new handshakes only; established sessions are untouched.
- [V] `generate` is minted once per process: `ServeCerts::generated` returns the existing cert on
  reload — "a fresh self-signed leaf would break anyone who pinned the old one" (`tls.rs:2823-2835`).
- [V] Generated parameters (`tls.rs:1101-1119`): `rcgen::KeyPair::generate()` — which in rcgen
  0.14.10 is `generate_for(&PKCS_ECDSA_P256_SHA256)` (`rcgen/src/key_pair.rs:83-87`) —
  `not_before = now_utc() - 1 day`, `not_after = not_before + 14 days`, self-signed; the doc
  comment: "Valid for two weeks starting yesterday: WebTransport refuses anything longer, and the
  backdating absorbs clock drift". `CertificateParams::new` parses each hostname as an IP SAN when
  it parses as an `IpAddr`, else a DNS SAN (`rcgen/src/certificate.rs:116-118`).
- [V] Certificate selection (`tls.rs:2881-2906`, `select`): the first cert whose leaf passes
  webpki `verify_is_valid_for_subject_name(sni)`; with no SNI or no match, "Serve the first one and
  let it decide whether to trust it". An IP-literal URL sends no SNI, so it always gets the first
  configured pair.
- [V] `/certificate.sha256` (`moq-relay/src/web.rs:778-789`): "The first certificate in
  configuration order, deliberately. The endpoint exists so an `http://` client can pin a
  self-signed development certificate, where there is exactly one." 404 "no certificate" for a
  stream-only relay. Served on `web.http.listen` (TCP); README: "The HTTP server listens on the
  same bind address, but TCP instead of UDP. The default is `http://localhost:4443`." A
  `web.https.{listen,cert,key,root}` listener also exists in source (`web.rs:91-150`) although the
  README still says "HTTPS is currently not supported".
- [V] Native clients bootstrap the same way (`Connect::allows_http_bootstrap`, `tls.rs:918-929`).

### 3. Programmatic issuance for a machine with a reachable hostname

- [V] Let's Encrypt challenge types (https://letsencrypt.org/docs/challenge-types/): HTTP-01 "can
  only be done on port 80", "cannot be used to issue wildcard certificates", "can be used to validate
  IP Addresses"; TLS-ALPN-01 "performed via TLS on port 443", "ACME client support is limited";
  DNS-01 "You can use this challenge to validate domain names whose webservers aren't exposed to
  the public internet", "Keeping API credentials on your web server is risky", "cannot be used to
  validate IP Addresses". [I] A LAN-only host therefore has exactly one public-CA route: DNS-01.
- [V] Profiles (https://letsencrypt.org/docs/profiles/): `classic` 90 days (default, DNS only),
  `tlsserver` 45 days (DNS only), `shortlived` 160 hours "6ish days", identifiers "DNS, IP". IP
  certificates validate only by "http-01 and tls-alpn-01" (https://letsencrypt.org/2025/01/16/6-day-and-ip-certs/,
  GA per its 2026-01-15 banner) — so an IP cert needs a *public* IP reachable on 80/443.
- [V] Rate limits (https://letsencrypt.org/docs/rate-limits/): "Up to 50 certificates can be issued
  per registered domain … every 7 days" (registered domain per the Public Suffix List), "Up to 5
  certificates … per exact same set of identifiers every 7 days", ARI-coordinated renewals exempt.
  [I] One Tatolab zone issuing per-machine certs is capped at ~50 new machines/week unless the
  zone is on the PSL or an override is granted.
- [V] CA/Browser Forum BR §1.2.2: maximum subscriber-certificate validity 200 days from
  2026-03-15, 100 days from 2027-03-15, 47 days from 2029-03-15 (ballot SC-081v3). [I] Every
  public-cert shape needs a renewal loop.
- Rust crates (crates.io metadata read 2026-10-17):
  - [V] `instant-acme` 0.8.5 (updated 2026-02-24, https://github.com/djc/instant-acme): "Async
    pure-Rust ACME (RFC 8555) client"; `Account` / `NewOrder` / `Order` / `Authorizations` /
    `KeyAuthorization`; `ChallengeType::{Http01, Dns01, TlsAlpn01, DeviceAttest01, Unknown}`;
    profiles (`ProfileMeta`), external account binding, ARI (`RenewalInfo`). The caller does the
    DNS TXT write itself.
  - [V] `rustls-acme` 0.15.4 (updated 2026-08-11): TLS-ALPN-01 default, HTTP-01 alternative;
    high-level `AcmeConfig` + `Incoming` stream of TLS connections, low-level `AcmeState` +
    `ResolvesServerCertAcme`; "must implement both account and certificate caching". No DNS-01.
  - [V] `acme-lib` 0.9.1 (updated 2024-01-24): synchronous, "ACME draft spec 18", `http_challenge`
    and `dns_challenge`. `tokio-rustls-acme` 0.9.1 (2026-05) is the tokio port of rustls-acme.
- "Local domain" services:
  - [V] Tailscale `tailscale cert` (comparison only): Let's Encrypt via DNS-01 — "Tailscale
    creates a `*.ts.net` DNS TXT record for your nodes"; private key and ACME account key "are
    generated and stored locally, and Tailscale never sees them"; 90-day expiry, "you must renew
    them yourself" for file-based certs; machine names land in public CT logs
    (https://tailscale.com/kb/1153/enabling-https). This is the model for shape C below.
  - [V] Cloudflare API tokens can be scoped to "Edit zone DNS" on one zone, with optional TTL and
    client-IP filtering (https://developers.cloudflare.com/fundamentals/api/get-started/create-token/).
  - [V] getlocalcert.net: free `*.localcert.net` / `*.localhostcert.net` subdomains with DNS-01,
    but "we don't host any `A`, `AAAA`, `CNAME`, or `MX` DNS records publicly" except
    `*.localhostcert.net → 127.0.0.1`; "still very much an MVP" (operator post, 2024-07-11); the
    site refused connections on 2026-10-17.
  - [V] nip.io / sslip.io: IP-encoding names, "do not support wildcard certificates", HTTP-01
    recommended — needs a public host. localtls serves its LE wildcard's "private and public
    keys" over REST ("someone can do a man-in-the-middle attack!"). Neither fits a LAN relay.
  - [V] Caddy gives IPs and `localhost` / `.local` / `.internal` / `.home.arpa` its internal CA
    and installs that root into the system trust store, prompting for a password; mkcert likewise
    installs a root into system, NSS/Firefox and Java stores, with leaves of `AddDate(2, 3, 0)`
    and RSA-2048 unless `-ecdsa`, and mobile roots installed by hand. [I] A local-CA shape costs a
    trust-store install on every viewer device and its leaves fail the pinning rules anyway.
- Baseline: short-lived self-issued ECDSA + pinning.
  - [V] Secure Contexts §3.1: potentially trustworthy origins are `https`/`wss`, `127.0.0.0/8`,
    `::1/128`, `localhost` and `*.localhost`, `file`, or a user-configured origin; anything else
    returns "Not Trustworthy" (https://w3c.github.io/webappsec-secure-contexts/). Chromium's
    list matches (https://www.chromium.org/Home/chromium-security/prefer-secure-origins-for-powerful-new-features/).
  - [V] `@moq/net` 0.4.3 `connection/connect.js:317-339`: when the relay URL is `http:`, fetch
    `/certificate.sha256`, push `{algorithm:"sha-256", value}` and rewrite the URL to `https:` —
    comment: "http:// can't be a real WebTransport origin … Production uses https://". Callers may
    also pass `serverCertificateHashes` (hex accepted) or `serverCertificate` (PEM/DER, hashed by
    `certificateHash`).
  - [I] Cross-machine over plain `http://<lan-ip>:4445`: the page's origin is not potentially
    trustworthy, so `WebTransport` is not exposed and the player never runs; the relay URL rewrite
    is irrelevant. It works from `http://localhost` / `http://127.0.0.1` only. Escape hatches: the
    Chrome flag `chrome://flags/#unsafely-treat-insecure-origin-as-secure` /
    `--unsafely-treat-insecure-origin-as-secure="http://…"` [V,
    https://www.chromium.org/Home/chromium-security/deprecating-powerful-features-on-insecure-origins/],
    or serving the preview page and `/certificate.sha256` over `https://` with the same
    self-signed cert so the origin is `https` after the user clicks through the interstitial
    (`web.https.*` exists for the fingerprint fetch). [I] Whether a fetch to a clicked-through
    self-signed origin succeeds from that page in every browser was not tested.
  - What it cannot do: a URL any browser on the LAN opens without a flag, an interstitial, or a
    pre-installed root.

### 4. What hosted relays and the player expect

- [V] Cloudflare MoQ: `https://draft-16.cloudflare.mediaoverquic.com/<token>` (draft-14 and
  draft-16; draft-18 in progress), auth token in the URL path, "free to use during the beta"
  (https://developers.cloudflare.com/moq/, https://blog.cloudflare.com/moq-relays/). Neither page
  states the certificate story. [I] A public Cloudflare hostname is served with an ordinary
  publicly trusted certificate, so the player uses empty hashes and plain PKI.
- [U] moq.pro: the page body is only the title "MoQ Pro"; nothing about a relay, pricing or TLS
  was readable, and no search result describes it.
- [V] moq.dev docs: the demo relay "runs with a generated certificate and anonymous access"; the
  public test relay is `https://cdn.moq.dev/anon`; the relay-setup guide's production section
  points at certbot files under `/etc/letsencrypt/live/<host>/` and restarts on renewal.
- [V] The player's contract (§3): `https://` → ordinary PKI unless hashes are supplied; `http://`
  → fingerprint bootstrap. A local relay matches a hosted one by serving a publicly trusted cert
  under a DNS name the URL uses; it matches the dev model by serving a ≤14-day ECDSA P-256 cert.

### 5. Recommendation space (not a decision)

- **A. Self-issued ECDSA P-256, ≤14 days, pinned.** `tatolabd` mints (rcgen or equivalent) a cert
  per listener with SANs for `localhost`, `127.0.0.1`, the hostname and its IPs, writes PEM pairs,
  rotates by atomic rename on a cadence shorter than 14 days (e.g. every 7, backdated 1), and the
  relay reloads. Cost: near zero; no network, no secrets, no names. Limit: on-machine browsers
  only, unless the owner accepts the `https://`-interstitial or Chrome-flag path for LAN viewers;
  a rotation also invalidates an earlier-fetched fingerprint, so the page refetches on reconnect.
- **B. Public DNS name per machine, DNS-01 with a provider token held locally.** `instant-acme`
  against Let's Encrypt (`classic` 90 d or `tlsserver` 45 d), the machine writes
  `_acme-challenge.<machine>.<zone>` through the provider API and publishes an `A`/`AAAA` record
  with its LAN address; the every-interface listener gets the file pair and the preview URL is
  `https://<machine>.<zone>:4445`. Cost: a zone, a scoped token in the OS secret store on every
  machine, a renewal loop, hostnames in CT logs, the 50/week registered-domain cap, resolvers
  that filter private answers [U], and A is still needed for the loopback listener.
- **C. Tatolab-operated zone doing DNS-01 on the machine's behalf (the `tailscale cert` model).**
  A Tatolab service owns `<zone>`, authenticates the machine, writes the TXT record and the LAN
  `A` record; the machine runs ACME itself so keys never leave it. Same browser result as B
  without a per-machine provider token; cost moves to running and securing that service plus
  machine identity, and the same CT / rate-limit / resolver caveats apply.

What the owner has to decide: whether a public, CT-logged DNS name per machine is acceptable;
whether a provider token may live in each machine's OS secret store (B) or issuance belongs to a
Tatolab service (C); whether on-machine-only pinning (A) is the MVP and LAN viewing waits; and, if
A ships alone, whether the `https://` self-signed page (interstitial) is an acceptable LAN path.
These are `/align` questions.

## What remains unknown

- WebKit's key-type policy for pinned certs (the PR shows only the two-week and SHA-256 checks)
  and Safari's exact shipping version (caniuse says 26.4 — secondary).
- Whether a `/certificate.sha256` fetch from a clicked-through self-signed `https://` page
  succeeds in Firefox and Safari as in Chrome; not tested.
- What moq.pro is; whether Cloudflare's MoQ hostnames carry anything but a standard public cert.
- Whether common home resolvers drop public-DNS answers pointing at private addresses (B, C).
- getlocalcert.net's current status (connection refused on 2026-10-17).
