# Web2Codex

Web2Codex is a local coding-agent harness fork. It keeps the terminal UI, permissions, sandbox, native tools, and subagents. **The private ChatGPT WebView is required for every model request**: it is the only inference path. API and OAuth model login, upstream Codex Cloud, the ChatGPT Desktop launcher, and the standalone cloud executor have been removed.

The WebView has a separate sign-in profile and uses Temporary Chats. A Temporary Chat is retained for each Codex thread, so follow-up requests send only new context; compaction starts a new chat, and concurrent agent threads stay isolated. The controller started for a harness session closes with the last harness window. A standalone `web2codex web open`, `web2codex web login`, or `web2codex web start` keeps a temporary controller available for up to ten minutes so a harness can attach after sign-in; it then closes automatically if no harness attaches. Once attached, it closes shortly after the final harness window exits. Web2Codex does not launch at computer startup.

**Status: prototype.** GitHub Actions builds and smoke-checks release packages for Windows, Linux, and macOS on x64 and ARM64. The signed-in GPT-6 browser smoke test returned the exact requested response, and a live Web2Codex MCP request discovered tools and ran `exec_command` successfully. The WebView supplies the TUI model list from labels visible in the signed-in ChatGPT picker and verifies the requested choice before sending. Requests that expose native tools select the saved custom connector pill in a Temporary Chat. ChatGPT calls that connector through OpenAI Tunnel; the bridge turns each MCP request into the matching native Web2Codex tool call and returns its result to the same ChatGPT response. Requests with no tools, or with tools disabled, do not attach the connector. The chat stays alive through all tool rounds. Tool schemas travel through MCP, and user-facing answers remain ordinary prose. Responses events return tools and completed text to the terminal. Local skills and configured external MCP tools use the Web2Codex tool router. Inline images and common audio formats are converted to composer attachments. Stable model IDs and exact token counts are unavailable. Inline media returned by native MCP tools is preserved.

## Features

- Native CLI builds for Windows, Linux, and macOS on x64 and ARM64.
- `web2codex` as the installed command on every platform.
- A private native WebView for ChatGPT. The account chooser offers Free/Go, Plus, and Pro; this records your choice but does not verify the account tier.
- Temporary Chat as the browser's default new-chat mode.
- One retained Temporary Chat per Codex thread, with new requests sending only the unprocessed context suffix and compaction starting a fresh chat.
- Model discovery from the signed-in ChatGPT Web picker, with exact-label selection before a request.
- Local usage statistics, without displaying ChatGPT account quota limits.
- OpenAI Secure MCP Tunnel linking the ChatGPT connector to the complete native Web2Codex tool registry.
- Native function calls, freeform patches, tool discovery, configured MCP tools, approvals, and sandboxing through the Web2Codex router.
- Independent hidden Temporary Chats for concurrent agent turns, sharing the same sign-in profile.
- GitHub Releases installers, SHA-256 verification, and the `web2codex update` command.

## Install

The installers below download the latest [GitHub Release](https://github.com/angyedz/web2codex/releases/latest).

**Linux and macOS**

```sh
curl -fsSL https://github.com/angyedz/web2codex/releases/latest/download/install.sh | sh
```

On Linux, the installer selects the musl build by default. To install the glibc
build instead, pass `WEB2CODEX_LINUX_LIBC=gnu` to the installer:

```sh
curl -fsSL https://github.com/angyedz/web2codex/releases/latest/download/install.sh | WEB2CODEX_LINUX_LIBC=gnu sh
```

**Windows PowerShell**

```powershell
irm https://github.com/angyedz/web2codex/releases/latest/download/install.ps1 | iex
```

Open a new terminal after installation, then start the agent:

```sh
web2codex
```

Web2Codex keeps its settings, sessions, browser profile, and installer releases in its own data directories (`~/.web2codex` and `~/.local/share/web2codex/browser` on Linux; `~/Library/Application Support/Web2Codex/Browser` on macOS; `%USERPROFILE%\.web2codex` and `%LOCALAPPDATA%\Web2Codex\Browser` on Windows). Set `WEB2CODEX_HOME` or `WEB2CODEX_WEB_DATA_DIR` to choose custom directories.

Starting `web2codex` offers first-run OpenAI Tunnel setup. On first use, the private ChatGPT window lets you choose Free/Go, Plus, or Pro and sign in; the terminal waits up to five minutes for the model picker to load. Later launches use the saved model catalog and keep the browser hidden. A browser started by the harness closes after its last terminal session disconnects, including after a terminal crash. A standalone `web2codex web start` hides the browser and gives a harness up to ten minutes to attach; after that, or after the attached harness exits, the browser closes automatically. Model requests use this Web session; Web2Codex does not use API keys or OAuth for inference. To update an installed release, run `web2codex update`.

## ChatGPT Web

The WebView uses the operating system's native browser component. Its account session is stored in a separate local profile.

```sh
web2codex web                 # Choose an account on first use; then open ChatGPT
web2codex web login           # Show the Free/Go, Plus, and Pro account choices
web2codex web open            # Open ChatGPT in the private window
web2codex web models          # Show the last observed ChatGPT Web model picker
web2codex web status          # Check the background controller
web2codex web hide            # Hide the window; keep the browser available in the tray
web2codex web stop            # Stop the controller
```

The first `web2codex web` opens the account chooser. Later runs start ChatGPT in Temporary Chat mode. The private WebView returns to the Temporary Chat entry if ChatGPT tries to open a saved-chat route, preserves the mode during new-chat navigation, and blocks the recognized mode switch. ChatGPT controls the actual server-side conversation behavior.

The chooser records the account type you selected, but does not verify your subscription. `web2codex web models` reports visible model-picker labels from ChatGPT Web. Web2Codex uses those labels as temporary model keys in the TUI and verifies the matching picker choice before sending; the labels are not stable model IDs or proof of account entitlement. ChatGPT decides which models the signed-in account can use.

`/usage [daily|weekly|cumulative]` reports local request counts, input and output character counts, average request time, and model selections. It does not query ChatGPT account analytics or display subscription limits. ChatGPT Web does not expose exact token counts to this harness; the local report stores only aggregate counts and never saves prompts or replies.

`/context` shows the current context profile. Use `/context stable`, `/context expanded`, or `/context unstable` to set the context window for all detected web models to 200k, 270k, or 400k tokens. Automatic compaction starts around 90% based on Web2Codex's local history estimate; ChatGPT Web does not expose exact token counts, so the threshold is approximate.

Inside the terminal chat, `/web` opens the private ChatGPT window. Use `/web login` to sign in again, `/web hide` to hide the window, `/web models` to view the saved web model list, or `/web status` to check the controller. Tunnel setup is available through `/mcp tunnel`.

The browser window has tabs, an address bar, navigation controls and refresh. To
create a background tab from a shell or a Web2Codex tool call, use
`web2codex web tab open https://example.com`; `web2codex web tab list` shows the
open tab URLs. Tab numbers are current positions and may change when tabs close.
Inference uses separate WebViews so its conversations do not share a tab.

The WebView is required for model requests. `web hide` hides its window while keeping the controller available to the current session. `web stop` stops the controller immediately; the next `web2codex` launch starts it again. The WebView does not launch at computer startup.

### Browser requirements

- **Windows:** Microsoft Edge WebView2 Runtime. It is included with Windows 11. If a supported Windows installation does not have it, the installer offers to download the small Microsoft bootstrapper and install the matching runtime after you confirm; installation works per-user and does not require bundling the 250+ MB fixed runtime with Web2Codex. In non-interactive mode, install the runtime from [Microsoft's WebView2 download page](https://developer.microsoft.com/microsoft-edge/webview2/) and rerun the installer.
- **macOS:** built-in WKWebView.
- **Linux:** glibc, GTK 3, WebKitGTK 4.1, and libxdo runtime libraries. These system libraries are not bundled with the release. The installer checks the browser's shared-library dependencies before activating it. On Debian/Ubuntu, Fedora, and Arch Linux it offers to install the required packages only after you confirm; in non-interactive mode it prints the package command and stops so you can install them yourself. On other distributions, install the packages providing GTK 3, WebKitGTK 4.1, and libxdo, then rerun the installer.

  ```sh
  # Debian / Ubuntu
  sudo apt-get install libwebkit2gtk-4.1-0 libxdo3

  # Fedora
  sudo dnf install webkit2gtk4.1 libxdo

  # Arch Linux
  sudo pacman -S --needed webkit2gtk-4.1 xdotool
  ```

## MCP tunnel

On Windows, run `start.cmd` from the project folder to launch Web2Codex. To
set up or replace tunnel credentials, run `setup.cmd` or enter `/setup` inside
Web2Codex. `setup.cmd` opens the main TUI directly in the native setup wizard.
The wizard opens the persistent Web2Codex browser for sign-in, then
lets you keep a generated two-word connector name or enter your own. It opens
the organization Tunnels and API Keys pages in browser tabs and guides you
through each field. Create a runtime key with expiration **Never** and
**Tunnels Read + Use** permissions; the key stays hidden while you paste it.

On the first interactive launch, the native TUI setup wizard opens until setup
is completed or skipped. Existing tunnel credentials can be reused. It stores
the tunnel ID and runtime API key in Web2Codex's encrypted secret store,
downloads and verifies the OpenAI Secure MCP Tunnel client, then starts it in
the background. After that it opens ChatGPT's connector settings and shows the
exact name and tunnel ID to enter.
Add a custom MCP connector, choose **Connection: Tunnel**, and save and enable
it. Then open **Settings > Plugins > Cosmic Birch > Permissions** and choose
**Allow all tools (High risk)** so the installed plugin can run connector actions
without stopping for approval each time. Press `T` in the setup wizard to run the browser smoke test. The test opens
an ephemeral Temporary Chat and checks up to four distinct models visible in
the signed-in picker, sending `Reply with exactly: READY TO WORK.` to each. It
uses an isolated temporary workspace and ignores user configuration and rules,
so project instructions and configured MCP servers are not loaded into the test
turn. It passes only if every checked model replies exactly `READY TO WORK.`;
any untested visible models are reported as unchecked. To run it later, use
`web2codex web tunnel smoke-test`. If you skip setup, use `/setup` in Web2Codex
later. On later launches, Web2Codex starts the configured tunnel automatically
for the current workspace.

After setup, Web2Codex prints the saved connector name and tunnel ID. In ChatGPT, add a custom MCP connector with exactly that name, select **Connection: Tunnel**, and enable the connector in Developer Mode. Temporary Chats must use **Personalized** mode for the connector to appear. The browser selects the saved connector pill from the `@` menu and checks it before sending each new prompt.

ChatGPT discovers the active turn's tools through `codex_tool_inventory` and invokes them through `codex_tool_call`. The bridge hands each native call to Web2Codex with its existing workspace, sandbox, and approval prompts. If the result takes longer than 12 seconds, ChatGPT gets a call ID and collects the result through `codex_tool_result` in the same Temporary Chat. Each connector request carries that turn's token.

Skill catalogs are discovered only when a skill fits the task. Web2Codex lists matching installed skills and reads the selected instructions through MCP; project `AGENTS.md` instructions remain part of each request.

The tunnel key is not put in the tunnel client's environment. A short-lived private file is used during startup and removed after the tunnel is ready. Concurrent turns are isolated by separate hidden WebViews and turn tokens. The tunnel can serve multiple workspaces because each tool executes in its own native Web2Codex context. See [the bridge architecture](docs/web2codex-bridge.md) for the protocol and local checks.

### External MCP servers

External servers are added to Web2Codex's own configuration with the normal MCP CLI. For an HTTP server that reads an API key from a custom header, define the secret in the environment used to launch Web2Codex and pass only the variable name:

```powershell
$env:PROJECTS_MCP_TOKEN = "your-token"
web2codex mcp add projects --url https://mcp.example.com/mcp --env-header Authorization=PROJECTS_MCP_TOKEN
```

Use `--bearer-token-env-var TOKEN_NAME` when the server expects standard bearer authentication. The secret value is never written to `config.toml`. HTTP MCP servers must use Streamable HTTP; local process servers can be added with `web2codex mcp add <name> -- <command> [args...]`. These MCP tools still execute through Web2Codex's native approval and sandbox rules. In an active chat, MCP discovery, server management commands, and calls to external server tools all go through the Web2Codex connector.

## Build from source

Install Rust 1.95.0 and platform build tools. On Linux, install `build-essential`, `pkg-config`, GTK 3, WebKitGTK 4.1, and `libxdo`; on macOS, install Xcode Command Line Tools; on Windows, use the MSVC Rust toolchain and Visual Studio C++ Build Tools. Running the browser also requires the operating-system WebView runtime listed above.

From the repository root:

```sh
cargo build --locked --release --manifest-path codex-rs/Cargo.toml -p codex-cli --bin web2codex
cargo build --locked --release --manifest-path codex-rs/Cargo.toml -p codex-web-browser --bin codex-web-browser
```

To publish a release, push a `codex-web-vX.Y.Z` tag. GitHub Actions packages Linux x64/ARM64 musl and glibc variants, Windows x64/ARM64, and macOS Intel/Apple Silicon builds, then publishes installers and checksums. The release workflow also configures the updater to use this repository.

## Project background

Web2Codex is based on the Apache-2.0-licensed [upstream OpenAI Codex repository](https://github.com/openai/codex). The fork retains its local agent loop, terminal interaction, sandboxing, approvals, tools, and subagents, while replacing hosted/API inference with the private ChatGPT Web session.
