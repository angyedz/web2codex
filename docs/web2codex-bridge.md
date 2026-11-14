# ChatGPT Web to native Codex tools

Web2Codex uses the connector workflow from [chatgpt-web-coder](https://github.com/angyedz/chatgpt-web-coder) and the turn broker architecture from [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web). Inference comes exclusively from the signed-in ChatGPT WebView.

## Request lifecycle

1. Codex sends its text context and native tool registry to the authenticated local `/v1/responses` endpoint. The registry stays in the broker; it is never appended to the ChatGPT prompt.
2. The broker creates a random turn capability and a hidden WebView. The view opens a fresh Temporary Chat, selects the requested model, selects the actual `@Web2Codex` connector pill, and submits the text with the capability token.
3. ChatGPT discovers native tools through the connector's `codex_tool_inventory`. Native descriptions and schemas are returned as MCP data. They are not instructions pasted into the composer.
4. ChatGPT invokes `codex_tool_call` through OpenAI Tunnel. Its MCP process forwards the token, exact `wire_name`, and arguments to the broker. This process cannot execute shell commands or manipulate workspace files.
5. The broker validates the token and tool against that turn's registry, allocates a `call_id`, and returns a native Responses `function_call`, `custom_tool_call`, or `tool_search_call` to Codex. The MCP request remains pending.
6. Codex's normal tool router executes the call with the current workspace, sandbox, approvals, and native lifecycle events.
7. The next Responses request contains the matching tool output. The broker correlates its `call_id`, completes the pending MCP request, and returns the result to ChatGPT. This request does not submit another web prompt or create another chat.
8. ChatGPT can invoke more tools in the same response. On completion, the browser checks that local tools have settled and that its observed tool revision is current. The final prose returns through Responses events; the broker retires the token and releases the hidden WebView.

Each concurrent agent turn has its own WebView, composer, pending calls, and token. Views share only the sign-in profile. A parent waiting for a child does not hold a global browser lock. Ordinary inference never shows or focuses the browser window.

## Connector contract

Create a custom connector in ChatGPT named **Web2Codex**, using **Connection: Tunnel** and the ID displayed by `/mcp tunnel`. Enable Developer Mode. Temporary Chats must use Personalized mode so the connector is available; the browser prepares this mode before selecting the mention.

- `codex_tool_inventory(turn_token, query?, offset?, limit?, include_schema?)` returns paginated native tool declarations, exact `wire_name` values, and native discovery entries.
- `codex_tool_call(turn_token, wire_name, arguments?, input?)` invokes the discovered tool. Functions and tool search accept an arguments object; freeform tools such as `apply_patch` accept their original input text.

Tool search results extend the registry on the next Codex request. Native text, structured MCP results, and inline image/audio content return through the connector. The bridge does not download media URLs. Text emitted by ChatGPT is always text: there is no sentinel parser and no request to format answers as JSON.

For the local bridge, failed native tool results carry a private transport marker so the connector can preserve the MCP `isError` flag. The marker is removed before the result reaches ChatGPT and is not used to parse model responses.

Inline PNG, JPEG, WebP, and GIF images plus WAV, MP3, M4A, AAC, OGG, WebM, and FLAC audio in user messages are attached to the ChatGPT composer. Media returned by native MCP tools is attached to the next Temporary Chat as well. The entire request is limited to 32 MiB, and remote media URLs are rejected with an explicit error. The browser waits for attachment cards before sending the prompt. Whether ChatGPT accepts a given audio format depends on its current Web upload support and has not yet been verified in a signed-in session.

The native catalog enables Codex terminal sessions and freeform patches because the bridge implements those call types. This describes harness functionality; it does not infer ChatGPT account entitlements from a model label.

## Cancellation and deadlines

The local socket listens on loopback and requires a random bearer credential from the private browser state file. This credential is separate from the per-turn capability given to ChatGPT and from the OpenAI Tunnel runtime key.

A disconnected Responses observer retires its turn and releases the browser. A cancelled or expired MCP invocation fails the turn and wakes its waiters. Late tool results cannot create a replacement chat. Provider retries are disabled to avoid silently submitting a second prompt after a transport failure.

MCP calls settle within 90 seconds, ahead of the tunnel transport deadline used by the reference project. Long commands should return a session and be polled in separate calls; command polls are bounded to 60 seconds and agent polls to 30 seconds. A timeout is reported without repeating the operation. Native approvals remain visible in Codex and count toward this transport deadline.

Explicit `web hide` and closing the user-facing browser hide that window while active hidden turns continue. `web stop` cancels all turns. Browser state protocol version 2 replaces the previous text-marker bridge on the next launch. The tunnel runtime signature changes with the executable so an older connector host is restarted after an update.

## Local verification

The broker and request-parser tests cover repeated native tool rounds in one browser turn, freeform patches, inline media, concurrent turn isolation, cancellation, completion fencing, native tool discovery, stale outputs, and exclusion of tool schemas from prompts.

```sh
cargo test --locked --manifest-path codex-rs/Cargo.toml -p codex-web-browser
node --test codex-rs/web-browser/tests/*.test.cjs
```

These checks do not sign in to ChatGPT, start a real tunnel, or verify account-specific browser selectors. A signed-in test with the configured connector is still required before calling this a verified application release. Streaming Responses requests receive assistant text as it appears in the ChatGPT page; non-streaming requests receive the completed response. Text before an MCP call is included alongside that call in the same Responses output.
