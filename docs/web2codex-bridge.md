# ChatGPT Web to native Web2Codex tools

Web2Codex uses the connector workflow from [chatgpt-web-coder](https://github.com/angyedz/chatgpt-web-coder) and the turn broker architecture from [codex-chatgpt-web](https://github.com/miuuyy/codex-chatgpt-web). Inference comes exclusively from the signed-in ChatGPT WebView.

## Request lifecycle

1. Web2Codex sends its text context and native tool registry to the authenticated local `/v1/responses` endpoint. The registry stays in the broker; it is never appended to the ChatGPT prompt.
2. The broker keeps one Temporary Chat per Codex thread and compaction epoch in a hidden WebView. The first request creates the chat and selects the requested model and reasoning effort. Later requests in that thread send only the new suffix after the last assistant reply. Compaction starts a fresh chat from the compacted context. Each concurrent agent thread gets its own chat. If native tools are available, the view selects the saved custom connector pill and submits the turn capability with the text.
3. ChatGPT discovers native tools through the connector's `codex_tool_inventory`. Native descriptions and schemas are returned as MCP data. They are not instructions pasted into the composer.
4. ChatGPT invokes `codex_tool_call` through OpenAI Tunnel. Its MCP process forwards the token, exact `wire_name`, and arguments to the broker. This process cannot execute shell commands or manipulate workspace files.
5. The broker validates the token and tool against that turn's registry, allocates a `call_id`, and returns a native Responses `function_call`, `custom_tool_call`, or `tool_search_call` to Web2Codex.
6. Web2Codex's normal tool router executes the call with the current workspace, sandbox, approvals, and native lifecycle events.
7. The next Responses request contains the matching tool output. The broker correlates its `call_id` and returns the output to ChatGPT through the original call or a result poll in the same chat.
8. ChatGPT can invoke more tools in the same response. On completion, the browser checks that local tools have settled and that its observed tool revision is current. The final prose returns through Responses events and the broker retires the turn token. The primary Codex thread's WebView remains available for its next request; completed agent views close when their agent thread is done.

When a request disables tools or has no native callable tools, the browser skips the connector pill and does not include a turn capability in the prompt. This keeps `tool_choice: none` tool-free and lets ordinary model requests work without a configured MCP tunnel.

Each concurrent agent turn has its own WebView, composer, pending calls, and token. Views share only the sign-in profile. A parent waiting for a child does not hold a global browser lock. Ordinary inference never shows or focuses the browser window.

## Terminal response format

Every fresh Web2Codex chat receives a formatting instruction for the terminal UI. The browser bridge extracts visible text and line breaks from ChatGPT's rendered page; it does not retain the original Markdown source or ChatGPT-only layout. In particular, table cells flatten into tab-separated text, clickable link targets and citation-card details can be lost, and cards, widgets, canvases, generated visualizations, Markdown task-list checkboxes, and custom callout/admonition blocks cannot be transferred faithfully. Code fences and diagram or math markup such as Mermaid, Graphviz, PlantUML, SVG, and LaTeX/MathJax also lose their special rendering. The model is asked to use complete plain-text answers, explicit labels such as `Step 1:`, `Status:`, and visible source URLs, and to summarize web-only visual content in text. Tool use remains through MCP; arguments are never pasted into the visible prompt as JSON.

Before forwarding assistant text to the terminal, the browser bridge strips ANSI/OSC escape sequences and control characters from answers, reasoning headings, and bridge errors. Tabs and line breaks remain intact.

## Connector contract

Create a custom connector in ChatGPT using the saved name and tunnel ID shown by `/setup`, with **Connection: Tunnel**. Enable Developer Mode. Temporary Chats must use Personalized mode so the connector is available; the browser prepares this mode before selecting the mention.

- `codex_tool_inventory(turn_token, query?, offset?, limit?, include_schema?)` returns paginated native tool declarations, exact `wire_name` values, and native discovery entries.
- `codex_tool_call(turn_token, wire_name, arguments?, input?)` invokes the discovered tool. Functions and tool search accept an arguments object; freeform tools such as `apply_patch` accept their original input text.
- `codex_tool_result(turn_token, call_id)` collects the result of an already submitted native operation. If approval or execution is still pending, the connector returns the same call ID. The inventory lists outstanding poll handles in `pending_calls`.

Tool search results extend the registry on the next Web2Codex request. Native text, structured MCP results, and inline image/audio content return through the connector. The bridge does not download media URLs. Text emitted by ChatGPT is always text: there is no sentinel parser and no request to format answers as JSON.

For the local bridge, failed native tool results carry a private transport marker so the connector can preserve the MCP `isError` flag. The marker is removed before the result reaches ChatGPT and is not used to parse model responses.

Inline PNG, JPEG, WebP, and GIF images plus WAV, MP3, M4A, AAC, OGG, WebM, and FLAC audio in user messages are attached to the ChatGPT composer. Media returned by native MCP tools is attached to the next Temporary Chat as well. The entire request is limited to 32 MiB, and remote media URLs or opaque `file_id` references are rejected with an explicit error because this local bridge cannot resolve them. The browser waits for attachment cards before sending the prompt. Whether ChatGPT accepts a given audio format depends on its current Web upload support and has not yet been verified in a signed-in session.

The current user message gets priority within the eight distinct upload slots. Repeated references to identical media reuse a single uploaded file. Older attachments that exceed the slot budget receive an explicit unavailable marker in the reconstructed history. Previous native calls retain their input alongside their results so a fresh chat can reconstruct the operations already performed.

Compaction requires a non-empty summary from the compaction request itself before it replaces history. The terminal refreshes a local context estimate after web responses that have no server token usage. These estimates drive context display and compaction without recording estimated tokens as billed usage.

The native catalog enables Web2Codex terminal sessions and freeform patches because the bridge implements those call types. This describes harness functionality; it does not infer ChatGPT account entitlements from a model label.

## Cancellation and deadlines

The local socket listens on loopback and requires a random bearer credential from the private browser state file. This credential is separate from the per-turn capability given to ChatGPT and from the OpenAI Tunnel runtime key.

A disconnected Responses observer cancels its active browser request; the primary thread's Temporary Chat remains available, while completed agent views close with their agent thread. If the connector disconnects before dispatch, the broker cancels the queued invocation. After dispatch, the broker retains the call handle and any late result so it can be collected through the connector. Provider retries are disabled.

Each MCP request waits up to 12 seconds for a native result. After that, the connector returns a pending handle so Web2Codex can keep displaying its approval prompt while ChatGPT polls. A call expires after 30 minutes without a poll; its handle remains marked as outcome-unknown so the model cannot safely retry a side effect. An orphaned turn is retained for at most one hour. Once collected, the latest deferred result remains available for repeat polls until another deferred result replaces it or the turn expires. Completed result caches may be evicted to make room for new turns; pending calls keep their retained handles.

Explicit `web hide` and closing the user-facing browser hide that window while active hidden turns continue. `web stop` cancels all turns. Browser state protocol version 4 adds the deferred result endpoint. The launcher refuses mixed browser helper protocol versions; stop active sessions before upgrading, then restart the launcher so matching helpers start. The tunnel runtime signature changes with the executable so an older connector host is restarted after an update.

## Local verification

The broker and request-parser tests cover repeated native tool rounds in one browser turn, freeform patches, inline media, concurrent turn isolation, cancellation, completion fencing, native tool discovery, stale outputs, and exclusion of tool schemas from prompts. The setup smoke test uses an ephemeral Temporary Chat, selects a model visible in the signed-in picker, and accepts only the exact `READY TO WORK.` reply. It does not invoke or mention the MCP connector.

```sh
cargo test --locked --manifest-path codex-rs/Cargo.toml -p codex-web-browser
node --test codex-rs/web-browser/tests/*.test.cjs
```

These checks do not sign in to ChatGPT, start a real tunnel, or verify account-specific browser selectors. A signed-in test with the configured connector is still required before calling this a verified application release. Streaming Responses requests receive assistant text as it appears in the ChatGPT page; non-streaming requests receive the completed response. Text before an MCP call is included alongside that call in the same Responses output.
