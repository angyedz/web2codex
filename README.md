# Web2Codex

Web2Codex is a local coding-agent harness fork. It keeps the terminal UI, permissions, sandbox, native tools, and subagents. **The private ChatGPT WebView is required for every model request**: it is the only inference path. API and OAuth model login, Codex Cloud, the ChatGPT Desktop launcher, and the standalone cloud executor have been removed.

The WebView has a separate sign-in profile and uses Temporary Chats. Web2Codex starts its controller when an agent session needs it and keeps the window hidden during normal use. It does not launch at computer startup. Closing or stopping it prevents new model requests until it is started again.

**Status: prototype.** The WebView supplies the TUI model list from labels visible in the signed-in ChatGPT picker, and the bridge selects and verifies the exact requested label before sending. ChatGPT does not expose stable model IDs or detailed capabilities through that UI, so reasoning, image, and tool support are not inferred from the labels. Text conversation history is sent as a role-labelled transcript in a Temporary Chat, and generated text is streamed back to the TUI. Tool requests use a bounded control block that the bridge converts into a standard Responses function call; the core tool router then applies its usual routing, approvals, and sandboxing. The harness's direct Realtime voice API and OpenAI Apps file-upload path are disabled. Images, audio, stable model IDs, and exact token counts are not available through this bridge. A complete signed-in run has not yet been verified. There is no usable release yet.

## Features

- Native CLI builds for Windows, Linux, and macOS on x64 and ARM64.
- `web2codex` as the installed command on every platform.
- A private native WebView for ChatGPT. The account chooser offers Free/Go, Plus, and Pro; this records your choice but does not verify the account tier.
- Temporary Chat as the browser's default new-chat mode.
- Model discovery from the signed-in ChatGPT Web picker, with exact-label selection before a request.
- Local usage statistics, without displaying ChatGPT account quota limits.
- OpenAI Secure MCP Tunnel setup for workspace files and commands.
- GitHub Releases installers, SHA-256 verification, and the `web2codex update` command.

## Install

The installers below use the latest [GitHub Release](https://github.com/angyedz/web2codex/releases/latest). There is no published binary release yet; until the first release is available, build from source using the instructions below.

**Linux and macOS**

```sh
curl -fsSL https://github.com/angyedz/web2codex/releases/latest/download/install.sh | sh
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

The background controller does not open a window automatically. For first-time sign-in, run `web2codex web`; it opens the Free/Go, Plus, and Pro chooser, then ChatGPT for sign-in. Model requests use this Web session; Web2Codex does not use API keys or OAuth for inference. To update an installed release, run `web2codex update`.

## ChatGPT Web

The WebView uses the operating system's native browser component. Its account session is stored in a separate local profile.

```sh
web2codex web                 # Choose an account on first use; then open ChatGPT
web2codex web login           # Show the Free/Go, Plus, and Pro account choices
web2codex web open            # Open ChatGPT in the private window
web2codex web models          # Show the last observed ChatGPT Web model picker
web2codex web status          # Check the background controller
web2codex web hide            # Hide the window; keep the controller available
web2codex web stop            # Stop the controller
```

The first `web2codex web` opens the account chooser. Later runs start ChatGPT in Temporary Chat mode. The private WebView returns to the Temporary Chat entry if ChatGPT tries to open a saved-chat route, preserves the mode during new-chat navigation, and blocks the recognized mode switch. ChatGPT controls the actual server-side conversation behavior.

The chooser records the account type you selected, but does not verify your subscription. `web2codex web models` reports visible model-picker labels from ChatGPT Web. Web2Codex uses those labels as temporary model keys in the TUI and verifies the matching picker choice before sending; the labels are not stable model IDs or proof of account entitlement. ChatGPT decides which models the signed-in account can use.

`/usage [daily|weekly|cumulative]` reports local request counts, input and output character counts, average request time, and model selections. It does not query ChatGPT account analytics or display subscription limits. ChatGPT Web does not expose exact token counts to this harness; the local report stores only aggregate counts and never saves prompts or replies.

The WebView is required for model requests and stays hidden during normal use. Open it to sign in or inspect ChatGPT. `web hide` hides the window while keeping the controller available. `web stop` stops the controller; a later agent launch starts it again when needed. The WebView does not launch at computer startup.

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

Configure the tunnel from the running terminal app by entering `/mcp tunnel` in the chat. The setup asks for the tunnel ID and runtime API key, stores them in Web2Codex's encrypted secret store, downloads and verifies the OpenAI Secure MCP Tunnel client, then starts it in the background. On later launches, Web2Codex starts the configured tunnel automatically for the current workspace.

After setup, Web2Codex prints the connector name and tunnel ID. Add that connector in ChatGPT and select **Connection: Tunnel**. You only need to do that once in ChatGPT. The same workspace MCP server is also registered locally in the harness with tool approval prompts. During Web2Codex model requests, the bridge asks ChatGPT Web to use the harness tool declarations; it converts a complete, allowlisted control block into a Responses tool call, which the native router handles. The bridge does not ask ChatGPT to call account connectors directly.

The tunnel key is not put in the tunnel client's environment. A short-lived private file is used during startup and removed after the tunnel is ready. File tools are scoped to the workspace selected when the tunnel starts; command tools run through Web2Codex's platform sandbox.

## Build from source

Install Rust 1.95.0 and platform build tools. On Linux, install `build-essential`, `pkg-config`, GTK 3, WebKitGTK 4.1, and `libxdo`; on macOS, install Xcode Command Line Tools; on Windows, use the MSVC Rust toolchain and Visual Studio C++ Build Tools. Running the browser also requires the operating-system WebView runtime listed above.

From the repository root:

```sh
cargo build --locked --release --manifest-path codex-rs/Cargo.toml -p codex-cli --bin web2codex
cargo build --locked --release --manifest-path codex-rs/Cargo.toml -p codex-web-browser --bin codex-web-browser
```

To publish a release, push a `codex-web-vX.Y.Z` tag. GitHub Actions packages Linux x64/ARM64, Windows x64/ARM64, and macOS Intel/Apple Silicon builds, then publishes installers and checksums. The release workflow also configures the updater to use this repository.

## Project background

Web2Codex is based on the Apache-2.0-licensed [Codex repository](https://github.com/openai/codex). The fork retains its local agent loop, terminal interaction, sandboxing, approvals, tools, and subagents, while replacing hosted/API inference with the private ChatGPT Web session.
