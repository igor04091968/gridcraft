# Web agent runbook

> **Last updated:** 2026-11-05 · **Change:** minor (Pollinations Free default and labeled catalog; Pollinations HTTP 400 diagnostics; Apply confirmation and navigation to changed cells; built-in HTTPS/WSS hosting and browser reconnect; Pollinations settings; guarded actions)

## Scope

The browser agent works on the active workbook through the same authenticated control channel as MCP. The shared `gridcraft-agent` backend calls an OpenAI-compatible chat-completions endpoint. Desktop runs this backend in process on private ephemeral loopback ports; no external service is needed for normal desktop use. Browser storage never contains the API key. `agent.run` reads and proposes only; `agent.apply` executes the proposal shown to the user; `agent.undo` rolls back the last applied agent action only while it remains the latest workbook edit.

This first version makes one model call and proposes at most one command. The step limit is reserved for a future multi-step loop and does not make this version iterative. There is no task-cancellation button: closing the panel does not stop a request already running on the server. Apply accepts only `cell.set`, `range.setValues`, `home.bold`, `home.italic`, `home.underline`, `home.mergeCells` and `sheet.moveColumns`; the server validates the command and required move parameters rather than trusting browser JSON. Moving L before K uses `cols: "L:L", before: 10` (zero-based destination boundary).

Every proposal is bound to the open document UID, revision, window, active sheet and full selection captured together with the workbook data. Apply checks that context in the same UI pass that executes the command; any intervening edit or navigation to a different context requires a new proposal. An unfinished cell edit or open undo group blocks snapshot/apply. Context is supplied by GridCraft, never by the model.

Agent Undo checks the document UID, post-apply revision and the actual top history snapshot. A later manual edit, manual Undo/Redo or a different active document makes agent Undo unavailable; it returns an error without changing any workbook. Selection navigation alone still permits Undo. A failed command or a command that produces no undo step cannot claim an agent action. These checks are shared by desktop and WASM.

## Configure

For browser use, build gridcraft-cli and the web distribution. Set GRIDCRAFT_CONTROL_TOKEN to a random token of at least 32 characters. The AI Agent panel offers four profiles:

- Codex (OpenAI API): model gpt-6.1-sol; set GRIDCRAFT_AGENT_OPENAI_API_KEY. Optional server-side endpoint override: GRIDCRAFT_AGENT_OPENAI_BASE_URL; default https://api.openai.com/v1.
- Kimi: model kimi-k3.1-pro; set GRIDCRAFT_AGENT_KIMI_API_KEY. Optional server-side endpoint override: GRIDCRAFT_AGENT_KIMI_BASE_URL; default https://api.moonshot.ai/v1.
- DeepSeek (PlusVibe): free model deepseek-v4-flash-0731:free; set GRIDCRAFT_AGENT_PLUSVIBE_API_KEY. Optional server-side endpoint override: GRIDCRAFT_AGENT_PLUSVIBE_BASE_URL; default https://plusvibeapi.ru/v1. The account's available models and free quota are controlled by PlusVibe.
- Pollinations: default Free model `stealth/glyph-cluster`; configure the key and model in the panel as described below.

The key must come from the provider's API platform. ChatGPT/Codex and Kimi Code subscriptions are separate from their API billing. For OpenAI, Kimi and PlusVibe, keys and endpoint overrides belong in the server service environment. Pollinations also supports a private settings file configured from the panel; the browser sends a new key only over the authenticated connection and never stores it locally. Keep the legacy GRIDCRAFT_AGENT_API_KEY, GRIDCRAFT_AGENT_BASE_URL, and GRIDCRAFT_AGENT_MODEL variables for existing OpenAI-compatible local services that call agent.run without a provider. Restart the control service after changing its environment. For remote browsers, use the [built-in HTTPS/WSS server](wss-hosting.md) or a trusted TLS reverse proxy.

In **Automate → AI Agent**, connect through **Подключение веб-интерфейса**, choose a model, enter a task and run it. Legacy `?control=ws://127.0.0.1:8780&controlToken=...` URLs remain supported; the token grants workbook control, so do not publish URLs containing it. The initial response is a proposal. Review the command and arguments, then select **Применить**. The proposal remains visible until the reply confirms a write; errors and no-change replies retain it. A successful reply shows the cell/range when available. **Показать изменения** closes the agent panel, selects the target sheet/range and scrolls it into view. Actions without an explicit range reveal the current selection. It refuses navigation if another workbook is active. **Отменить последнее изменение** restores the agent action; selection navigation still permits guarded Undo. Nothing writes before Apply.

## Pollinations setup from the application

Open **Automate → AI Agent**, choose **Pollinations**, then expand **Настройка Pollinations**.
Enter your own API key from [enter.pollinations.ai](https://enter.pollinations.ai), choose a
model from the live catalog (or enter its ID), and select **Сохранить настройки**.
A blank key field retains the existing key; **Удалить сохранённый ключ** explicitly removes it.
**Проверить подключение** checks the entered key (or saved key when the input is blank) using `/account/key`, without saving changes or sending workbook
content or generating text. **Обновить модели** loads compatible text models from `/text/models`.
The default is `stealth/glyph-cluster` (Glyph Cluster, Free). The catalog puts Free models first and keeps all compatible paid models available. It labels metered Quest models as paid even when `paid_only` is false; missing/invalid pricing is shown as unknown. Existing explicitly saved models remain selected. There is no automatic fallback to a paid model.

Glyph Cluster is experimental, limited to 8 requests/minute in the catalog verified on 2026-11-05, and Pollinations says requests/responses may be retained and used for training. The panel displays these facts when selected. Qwen3Guard 8B (`qwen/qwen3guard-gen-8b`) is also Free but is a specialized text moderator: it does not produce spreadsheet commands in the required JSON format. It stays selectable with a specialization label; use Glyph Cluster for workbook tasks. Prices and availability are provider-controlled; refresh the catalog for current metadata.

The API endpoint is `https://gen.pollinations.ai/v1/chat/completions`. Provider API evidence:
[public OpenAPI reference](https://gen.pollinations.ai/openapi.json), verified 2026-11-04.
The same panel works on desktop and the web app. In the web panel, expand
**Подключение веб-интерфейса**, enter the server WebSocket URL and control token, and click
**Подключиться**. The token is cleared from the input. It is retained in `gridcraft.connection` only after successful authentication when **Запомнить на этом устройстве** is enabled; the next visit connects automatically. **Забыть токен** removes it. This optional browser storage is separate from provider preferences and is not encrypted. URLs with query parameters remain supported for existing deployments. Desktop starts the backend itself;
the web app requires the authenticated WebSocket backend (`gridcraft-cli web-control --config /absolute/path/to/remote.json` for built-in HTTPS/WSS hosting, or plain `gridcraft-cli web-control` behind a TLS proxy).
Remote browsers must use WSS, either through the built-in server or a trusted reverse proxy.

Settings live in `agent.json` in the user settings directory: `$XDG_CONFIG_HOME/gridcraft`
(or `~/.config/gridcraft`) on Linux/BSD, `~/Library/Application Support/GridCraft` on macOS,
and `%APPDATA%/GridCraft` on Windows. Unix files are created with mode 0600, existing access
restrictions are retained, symlink/non-private/oversized files are refused on read, and keys
never appear in RPC responses or UI/browser preference JSON. This is private file storage,
not OS-keychain encryption. `GRIDCRAFT_AGENT_CONFIG_PATH` selects an explicit backend path.
`GRIDCRAFT_AGENT_POLLINATIONS_API_KEY` remains an optional environment fallback; removing the
saved key does not remove a separately configured environment key.

Non-secret desktop selection is kept in `agent-ui.json`; browser selection stays in
`gridcraft.agent`. `agent.settings`, `agent.configure`, `agent.models` and `agent.test` all
require the authenticated connection and `provider: "pollinations"`. Example configure params
are `{ "provider": "pollinations", "model": "stealth/glyph-cluster", "apiKey": "<your own key>" }`.
`agent.configure` with `clearKey: true` removes the stored key, and a missing `apiKey` preserves it.
A reply contains provider/model and configured/storedKey booleans, never a credential.

For installations deliberately using an external desktop bridge, set
`GRIDCRAFT_AGENT_USE_CONTROL_SERVER=1` and retain the existing local port/token configuration.
The server-side `GRIDCRAFT_AGENT_POLLINATIONS_BASE_URL` override is available for controlled
testing/deployments; the UI cannot supply arbitrary API hosts. Catalog/auth checks always use
the official Pollinations host.

## Verify

1. Without agent environment variables, `agent.run` returns a readable configuration error and does not modify the workbook.
2. With a test OpenAI-compatible endpoint, a formula/value proposal appears before any write.
3. Apply updates the captured workbook and confirms the address. Show changes reveals offscreen cells and the target sheet; Undo still restores the previous value while preserving earlier manual edits. A failed or no-change reply retains the proposal. Repeated Apply/Undo is refused.
4. Changing the selection, sheet, window or document before Apply returns an error without a write. Editing and undoing that edit still invalidates the old revision.
5. A manual edit after Apply makes agent Undo return an error and leaves that manual edit intact.
6. MCP `read_range` sees the same active browser workbook.
7. Wrong browser or MCP tokens are rejected.
8. Reloading restores the selected provider/model from `gridcraft.agent`; that localStorage entry contains no API key or control token.

Verified 2026-11-05: `cargo xtask ci` passed all ten stages (2,825 Rust tests including doctests) with the optional craft-fonts checkout. Chromium exercised the final optimized WASM through the public authenticated WSS backend in an isolated synthetic workbook: Apply wrote Z500, Show changes selected and revealed it, and Undo restored the value. Stale context and invalid parameters left the workbook intact and retained the proposal; an actual no-op column move returned `applied: false` with the distinct no-change status. No page errors occurred. These checks use controlled proposals to verify execution and feedback, rather than claiming every AI-generated task is supported. A separate live Pollinations proposal for A1 had already confirmed that generation and Apply worked before the feedback fix.

Pollinations HTTP 400 now distinguishes an invalid model/alias, a model context limit and unsupported parameters when the provider supplies a recognized reason. For an invalid model, refresh **Обновить модели**, choose a catalog entry or enter the Free default `stealth/glyph-cluster`. For a context limit, narrow the range and task. Unrecognized reasons are reported explicitly as unknown; the workbook remains unchanged. HTTP 401/403, 402 and 429 retain their separate key/permissions, balance and rate-limit messages. Error bodies are capped at 64 KiB and classified locally; raw provider messages are never forwarded because they can echo credentials or cell contents.

Verified 2026-11-05: the HTTP 400 fix passed `cargo xtask ci` (all ten stages, 2,827 Rust tests including doctests). The HTTP regression test reads a real mock 400 body and then completes another request; classification tests prove that echoed credentials and workbook text do not enter replies. The updated public backend was checked in Chromium with an isolated workbook: an intentionally invalid model displayed the specific model/alias message, then the saved model generated a summary from long Russian cell text. The generated `cell.set` action applied its exact proposed text and guarded Undo restored the cell. The server used its saved API key; the browser supplied no provider key. No page errors occurred. The owner's original HTTP 400 was not reproduced with the saved model; the exact failing client model/task remains unknown.

Verified 2026-11-05: `cargo xtask ci` passed all ten stages (2,830 Rust tests including doctests); new regressions cover the Free default, retained explicit paid selection/key, pricing classification and UI labels. Both Free catalog entries were called with synthetic data. Through the public browser/WSS backend, Glyph Cluster generated a two-sentence Russian summary in about 20 seconds; Apply wrote exactly the proposed value and guarded Undo restored B1. Qwen3Guard answered in about 9 seconds but its moderation output was rejected as a spreadsheet proposal; no workbook edit occurred. A separate direct moderation request returned HTTP 200 and `Safety: Safe / Categories: None`. The browser supplied no provider API key and had no page errors.

## Recovery

Stop `gridcraft-web-control.service` to disconnect both agent and MCP. Remove `control` and `controlToken` from the URL to use the standalone editor. The workbook's ordinary File → Undo remains available after an applied proposal. Server logs must not include the token, API key or full workbook contents.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-05 | minor | Pollinations defaults to Free Glyph Cluster; live catalog retains paid models, sorts Free first and marks specialized moderation models |
| 2026-11-05 | minor | Bounded Pollinations HTTP 400 response classification distinguishes invalid model, context limit and unsupported parameters without exposing echoed credentials or workbook text |
| 2026-11-05 | minor | Apply retains proposals until success, distinguishes errors/no change, and offers Show changes without breaking agent Undo |
| 2026-11-04 | minor | Built-in HTTPS/WSS hosting with owner-only desktop management; bounded concurrent browser sessions; heartbeat/reconnect without replay |
| 2026-11-04 | minor | Pollinations key/model setup, catalog, connection check and removal in shared desktop/web panel; private backend settings and built-in desktop backend |
| 2026-11-04 | minor | Bind proposals to workbook context; agent Undo identifies its own history step; validation errors preserve the WebSocket connection |
