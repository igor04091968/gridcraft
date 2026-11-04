# Built-in HTTPS and WSS hosting

> **Last updated:** 2026-11-05 · **Change:** minor (M14.4)

GridCraft can serve its WASM application and agent WebSocket on the same native Rust TLS
listener. Python and a reverse proxy are not needed in this mode. The web page is at the
configured HTTPS origin, the agent at `/agent/ws`, and a minimal health response at `/health`.

## Desktop setup

1. Build the WASM distribution (`env NO_COLOR=false trunk build --release` in
   `apps/gridcraft-web`) and install a trusted certificate for your domain.
2. In **Automate → AI Agent → Удалённый доступ HTTPS / WSS**, enter the IP/port to listen
   on, HTTPS origin such as `https://grid.example.org:8443`, and absolute paths to the web
   directory (`dist/web`), certificate chain PEM and private key PEM. The origin has no
   trailing slash. The key must be a regular private file (0600 on Unix); PEM symlinks are
   refused. If a certificate tool uses symlinks, select a regular deployment copy.
3. Save, then start. Enable **Запускать при открытии GridCraft** for subsequent desktop
   launches. Stop before saving different server settings. Status includes the running flag,
   current clients and certificate reload errors; the pairing credential is omitted.
4. Use **Показать токен подключения → Копировать токен** to pair a browser. The field is
   masked and excluded from UI preferences and logs; hiding it or closing the panel clears
   it. Paste it in the browser's agent connection settings. The WSS address defaults to
   the page's own origin on HTTPS pages. Configure the Pollinations API key in its own panel.
5. **Сменить токен и отключить клиентов** stops existing connections and replaces the
   token. Pair again using the new token. Tokens are generated from 256 random bits and
   stored only in the private server configuration, not browser storage.

`remote.json` lives next to the native agent settings: `$XDG_CONFIG_HOME/gridcraft` (or
`~/.config/gridcraft`), `~/Library/Application Support/GridCraft`, or `%APPDATA%/GridCraft`.
`GRIDCRAFT_REMOTE_CONFIG_PATH` selects another path. It contains a credential: do not commit
or publish the file. The private key remains in its separate PEM file.

## Service mode

After saving the settings in the desktop panel, a CLI-only service can use the same file:

```sh
gridcraft-cli web-control --config /absolute/path/to/remote.json
```

The explicit service command starts the saved configuration regardless of the desktop
autostart checkbox. A supervisor such as systemd can run this command at login/boot and
restart it after a process failure. The desktop checkbox starts hosting only when the
desktop application itself is launched; it does not install a system service.

The legacy loopback `web-control --listen 127.0.0.1:8780 --port 7979` bridge remains for
local desktop/MCP compatibility. The new public WSS endpoint has independent agent-only
browser sessions and does not forward MCP to an arbitrary user's workbook. The service
mode occupies its listener itself: stop it before starting the same address from desktop.

## Certificates and recovery

The server validates PEM/key matching before opening its listener. New connections reload
the certificate and key, so an external certificate renewal/deployment job needs no server
restart. A malformed replacement fails closed and appears in server status; existing TLS
connections retain their negotiated certificate. Deploy a matching regular certificate/key
pair with private key permissions to recover. Browsers still validate issuer, domain and
expiry through their ordinary TLS trust store.

Certificate issuance is not included: a public domain, trusted certificate and network
reachability are deployment inputs. No ACME account or challenge mechanism is silently
configured, and self-signed test certificates are never used for public deployment.

Browser reconnect uses 1–30 second backoff for transport failures, stops after authentication
rejection, and retains the connection token only in tab memory. Explicit **Отключиться**
stops retries. In-flight requests and proposals are invalidated on disconnect; sent commands
are never automatically retried, because Apply may already have changed the workbook.
Inspect the workbook before submitting the task again. Reloading requires pairing again.

## Bounds and scope

- TLS handshake/authentication have timeouts. HTTP headers are capped at 16 KiB; WebSocket
  messages at 4 MiB; there are at most 16 simultaneous connections.
- WebSocket Origin must exactly match the configured HTTPS origin. HTTP Host must match
  its authority. The server exposes only a fixed set of web asset extensions under the
  configured directory; traversal, hidden paths, escaped paths and outside symlinks fail.
- The same owner token authorizes all clients. This does not provide accounts or tenant
  isolation for provider settings. Each browser's workbook request stream is separate.
- Each idle WSS connection receives protocol heartbeats. Server management and pairing are
  unavailable over the public endpoint; they are restricted to the authenticated local bridge.
- The workbook remains in the open desktop/browser UI. Hosting keeps the connection running;
  it does not execute background workbook jobs after the browser closes. The agent still
  proposes one reviewed action per model call. Multi-step automatic editing is a separate feature.

## Notebook deployment (2026-11-04)

The notebook already has a Let's Encrypt certificate for `notebook.iri1968.ru` in
`/etc/caddy/certs/{fullchain.cer,private.key}`. GridCraft uses private regular copies in
`/home/igor/.config/gridcraft/tls/` (0700 directory, 0600 files), deployed by the root-owned
`/usr/local/bin/gridcraft-tls-sync`. `gridcraft-tls-sync.path` watches the source directory;
`gridcraft-tls-sync.timer` reconciles it hourly. The original ACME installation remains the
certificate authority/deployment source. No keys are stored in this repository.

`gridcraft-https.service` runs the CLI as `igor`, with only `CAP_NET_BIND_SERVICE` to bind
443, waits for the project filesystem and restarts on failure. Its private configuration is
`/home/igor/.config/gridcraft/remote.json`; the distribution is `dist/web` and the service
binary is `dist/bin/gridcraft-cli-wss`. Desktop management acts on its own process: stop
this system service before hosting the same address from the desktop panel. Pairing through
the desktop panel reads the same private configuration.

The existing `notebook-ddns-acme-sync.timer` remains enabled. After the owner replaced the
rejected DNS token, read-only token/zone checks returned HTTP 200. The sync script now exports
its current `CF_TOKEN` as `CF_Token` for the ACME `dns_cf` child, preventing renewal from using
an obsolete saved credential. The existing service successfully renewed and installed the
certificate; the expiry is **2027-02-02 19:58:12 UTC**. The private GridCraft deployment copy
updated automatically, and a subsequent public HTTPS handshake used the new certificate
without restarting the WSS service. No credentials appear in this document.

The public port was blocked by UFW, not a missing router mapping. The only added network
rule is `ufw allow 443/tcp comment 'GridCraft HTTPS WSS'`; public HTTPS `/health` returns
200 with normal certificate validation. No VPN or default route was changed.

Rollback: stop and disable `gridcraft-https.service`, delete the added UFW 443 rule (`sudo ufw delete allow 443/tcp`), restore the web/binary copies from
`dist/deploy-backups/20261104T204033Z-wss/`, and restart the previous web-control service.
The former HTTP 8080 editor remains available; HTTPS is a separate browser origin and
cannot read its IndexedDB, so export/import any existing local workbooks when migrating.

## Verification

`cargo test -p gridcraft-agent` tests real TLS sockets with explicitly trusted synthetic
certificates: static assets, traversal protection, Origin rejection, token authentication,
remote-management refusal, concurrent workbook routing, stop/restart and certificate reload.
The egui test exercises configuration/start/stop/pairing and exclusion of credentials from
preferences and logs. Full `cargo xtask ci` passed all ten stages (2,818 Rust tests including doctests) with the existing optional craft-fonts checkout
(`CRAFT_FONTS_DIR=/mnt/usb_hdd1/Projects/craft-fonts`). The default strict font-only test run
failed on two unsupported glyphs; the pinned font input restored the intended test coverage.
Public Chromium checks passed with the normal browser trust store: the deployed WASM
workbook rendered without page errors, secure context,
WSS authentication, Pollinations settings RPC, rejection of an incorrect token, and refusal
of remote `server.status`. The browser reconnect test sends one invalid Apply, restarts its isolated server and checks
that only one Apply was sent across reauthentication. Explicit disconnect stops further
connections. The browser also verifies that an incorrect token causes exactly one new connection,
with no subsequent retries, and that legacy query credentials are removed from the URL.
The two shared-protocol authentication regression tests passed in native CI.
The authenticated public socket also survived 65 seconds of idle time and accepted a further
RPC, verifying protocol heartbeats. No provider generation or workbook writes were requested.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-05 | minor | Verified notebook deployment, renewed certificate, automatic TLS synchronization and public trusted WSS |
| 2026-11-04 | minor | Built-in single-origin HTTPS/WSS server, desktop settings, isolated browser channels, reconnect and operational bounds |
