# Pollinations agent verification — 2026-11-04

> **Last updated:** 2026-11-04 · **Change:** minor (provider settings and shared native backend)

The desktop and browser share the AI Agent panel. Pollinations offers a masked API-key
input, model ID and live catalog, save, credential check and explicit stored-key removal.
Desktop starts the backend in process; the browser connects through its server URL/token
fields to `gridcraft-cli web-control`. Provider credentials are held in the native backend.
Only provider/model preferences are persisted by the UI.

## API and isolated backend checks

- Official OpenAPI reference: <https://gen.pollinations.ai/openapi.json>.
- Chat endpoint: `https://gen.pollinations.ai/v1/chat/completions`; default alias `openai`.
- Live `/text/models`: 261–263 chat-compatible models returned across verification calls;
  the provider catalog changes over time.
- Live `/account/key`: an intentionally invalid test key returns a redacted authentication
  error. No real account key was used, and no paid provider generation was performed.
- An isolated local chat server verifies that generation uses the saved test key/model.
- Settings/configuration RPC replies omit the key. Unix settings file permissions are 0600.
- Storage is a private settings file, without OS-keychain encryption. Removal does not
  remove a separately configured environment fallback.

The local chat response proposes setting A1 to 42. Workbook-context and Undo guards are
shared with the existing agent review implementation. The runbook documents the allowed
commands and the current single-call/single-command scope.

## Automated provider and UI checks

`cargo xtask ci` passes all ten stages on the final sources: i18n-report, fmt, clippy,
workspace tests, doctests, assets, layers, WASM, parity and keytips. The optional pinned
`craft-fonts` checkout is supplied as the required font build input.

The full workspace test run confirms private settings reload/removal, bounded invalid-input
handling, separate provider defaults, safe RPC errors and continued connection usability.
Shared UI tests confirm accessible controls, save/remove requests and credential-free
preferences. The review tests cover explicit Apply, single-use proposals, stale document/
revision/selection rejection and Undo preserving later manual work.

The live desktop check uses the embedded backend without an external CLI service:
Pollinations loads the saved settings/catalog, Run displays the local model proposal while
A1 stays empty, clicking Apply sets A1 to 42, and agent Undo restores the empty cell.
Saving and removing the test key through desktop controls are verified against the isolated
settings file, including mode 0600 after save.

Playwright verifies the browser flow through real panel controls: server URL/token entry,
masked key save, an inert proposal, Apply setting A1 to 42, Undo restoring A1, reload
restoring Pollinations and the selected `gemini` model, and explicit key removal. No test
API key or control token appears in localStorage; browser page errors are empty. The same
isolated model response is used, with no real provider generation.
The browser sequence passes again on the final optimized `trunk build --release`
distribution. Native binaries build with `cargo build --locked --offline -p gridcraft`.
Verification artifacts and synthetic settings are under `/tmp`; user credentials and
preferences are not modified by these tests.

## Published deployment

The verified optimized distribution is deployed to `dist/web`, preserving previous hashed
assets for already open pages and replacing `index.html` atomically. The release CLI is
rebuilt and `gridcraft-web-control.service` restarted. The existing private/public HTTP
services and the backend are all active. A fresh browser on the published local HTTP
endpoint authenticates with the existing control token, opens Pollinations and loads the
live catalog with no RPC or page errors. The test does not write provider settings or
modify any user workbook.

Rollback files are under `dist/deploy-backups/20261104T200628Z/`: `web/` and the previous
`gridcraft-cli`. Restore the old web directory and CLI executable, then restart the control
service. Credentials are not included in the backup.

The existing public site uses HTTP. Remote agent connections require a trusted WSS endpoint;
this deployment does not add TLS or send provider keys over remote unencrypted WebSockets.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-04 | minor | Document official API and isolated Pollinations backend verification |
