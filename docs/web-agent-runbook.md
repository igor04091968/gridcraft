# Web agent runbook

> **Last updated:** 2026-11-05 · **Change:** minor (built-in HTTPS/WSS hosting and browser reconnect; Pollinations settings; guarded actions)

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
- Pollinations: default model alias `openai`; configure the key and model in the panel as described below.

The key must come from the provider's API platform. ChatGPT/Codex and Kimi Code subscriptions are separate from their API billing. For OpenAI, Kimi and PlusVibe, keys and endpoint overrides belong in the server service environment. Pollinations also supports a private settings file configured from the panel; the browser sends a new key only over the authenticated connection and never stores it locally. Keep the legacy GRIDCRAFT_AGENT_API_KEY, GRIDCRAFT_AGENT_BASE_URL, and GRIDCRAFT_AGENT_MODEL variables for existing OpenAI-compatible local services that call agent.run without a provider. Restart the control service after changing its environment. For remote browsers, use the [built-in HTTPS/WSS server](wss-hosting.md) or a trusted TLS reverse proxy.

In **Automate → AI Agent**, connect through **Подключение веб-интерфейса**, choose a model, enter a task and run it. Legacy `?control=ws://127.0.0.1:8780&controlToken=...` URLs remain supported; the token grants workbook control, so do not publish URLs containing it. The initial response is a proposal. Review the command and arguments, then Apply or Undo. Nothing writes before Apply.

## Pollinations setup from the application

Open **Automate → AI Agent**, choose **Pollinations**, then expand **Настройка Pollinations**.
Enter your own API key from [enter.pollinations.ai](https://enter.pollinations.ai), choose a
model from the live catalog (or enter its ID), and select **Сохранить настройки**.
A blank key field retains the existing key; **Удалить сохранённый ключ** explicitly removes it.
**Проверить подключение** checks the entered key (or saved key when the input is blank) using `/account/key`, without saving changes or sending workbook
content or generating text. **Обновить модели** loads compatible text models from `/text/models`.
The stable default is the `openai` alias, not a claim that a particular model stays available.

The API endpoint is `https://gen.pollinations.ai/v1/chat/completions`. Provider API evidence:
[public OpenAPI reference](https://gen.pollinations.ai/openapi.json), verified 2026-11-04.
The same panel works on desktop and the web app. In the web panel, expand
**Подключение веб-интерфейса**, enter the server WebSocket URL and control token, and click
**Подключиться**. The token is cleared from the input and is not persisted; URLs with query
parameters remain supported for existing deployments. Desktop starts the backend itself;
the web app still requires `gridcraft-cli web-control` and its authenticated WebSocket.
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
are `{ "provider": "pollinations", "model": "openai", "apiKey": "<your own key>" }`.
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
3. Apply updates the captured workbook; Undo restores its previous value while preserving earlier manual edits. Repeated Apply/Undo is refused.
4. Changing the selection, sheet, window or document before Apply returns an error without a write. Editing and undoing that edit still invalidates the old revision.
5. A manual edit after Apply makes agent Undo return an error and leaves that manual edit intact.
6. MCP `read_range` sees the same active browser workbook.
7. Wrong browser or MCP tokens are rejected.
8. Reloading restores the selected provider/model from `gridcraft.agent`; that localStorage entry contains no API key or control token.

## Recovery

Stop `gridcraft-web-control.service` to disconnect both agent and MCP. Remove `control` and `controlToken` from the URL to use the standalone editor. The workbook's ordinary File → Undo remains available after an applied proposal. Server logs must not include the token, API key or full workbook contents.

## Revision history

| Date | Change | Summary |
|---|---|---|
| 2026-11-04 | minor | Built-in HTTPS/WSS hosting with owner-only desktop management; bounded concurrent browser sessions; heartbeat/reconnect without replay |
| 2026-11-04 | minor | Pollinations key/model setup, catalog, connection check and removal in shared desktop/web panel; private backend settings and built-in desktop backend |
| 2026-11-04 | minor | Bind proposals to workbook context; agent Undo identifies its own history step; validation errors preserve the WebSocket connection |
