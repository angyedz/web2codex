# Web2Codex

Web2Codex is a cross-platform fork of the [OpenAI Codex CLI](https://github.com/openai/codex). It keeps Codex's terminal agent, model catalog, authentication, and native subagents, and adds an optional private ChatGPT Web window and an OpenAI Secure MCP Tunnel helper.

The terminal agent and browser are separate: they use separate conversations and the browser has its own sign-in profile. The browser controller stays hidden until you explicitly open it; it does not start with the computer.

## Features

- Native CLI builds for Windows, Linux, and macOS on x64 and ARM64.
- `web2codex` as the installed command on every platform.
- A private native WebView for ChatGPT with account-choice buttons for Free/Go, Plus, and Pro. The choice is only a reminder; ChatGPT decides the account's actual access.
- Temporary Chat as the browser's default new-chat mode.
- `/usage` session and token statistics, without ChatGPT account quota limits.
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

Sign in with a ChatGPT account or configure an API key when prompted. To update an installed release, run `web2codex update`.

## ChatGPT Web

The WebView uses the operating system's native browser component. Its account session is stored in a separate local profile.

```sh
web2codex web                 # Choose an account on first use; then open ChatGPT
web2codex web login           # Show the Free/Go, Plus, and Pro account choices
web2codex web open            # Open ChatGPT in the private window
web2codex web models          # Show the CLI catalog and last observed Web model picker
web2codex web status          # Check the background controller
web2codex web hide            # Hide the window and release WebView memory
web2codex web stop            # Stop the controller
```

The first `web2codex web` opens the account chooser. Later runs start ChatGPT in Temporary Chat mode. Opening ChatGPT again reloads the Temporary Chat entry; the browser also preserves that mode on its new-chat surface and blocks the recognized mode switch. ChatGPT controls the actual server-side conversation behavior.

The chooser records the account type you selected, but does not verify your subscription. `web2codex web models` lists the model catalog bundled with the CLI and the last model-picker text observed in ChatGPT Web. The browser snapshot is informational; ChatGPT decides which models the signed-in account can use.

The browser is optional. Closing or hiding it does not stop terminal sessions. It is not opened automatically at sign-in or startup.

### Browser requirements

- **Windows 11:** Microsoft Edge WebView2 Runtime. It is included with Windows 11; other supported Windows installations can use [Microsoft's WebView2 deployment guide](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution).
- **macOS:** built-in WKWebView.
- **Linux:** glibc, GTK 3, and WebKitGTK 4.1 runtime libraries. These system libraries are not bundled with the release.

## OpenAI Secure MCP Tunnel

Run setup from the workspace you want ChatGPT to access. The prompts accept the tunnel ID and API key, save them in Codex's encrypted secret store, install the official OpenAI tunnel client, verify its release digest, and start it in the background.

```sh
web2codex web tunnel configure
```

When setup completes, the CLI prints the connector name and tunnel ID. In ChatGPT, add an MCP connector with that name, select **Connection: Tunnel**, and enter the printed tunnel ID. The connector exposes workspace file read, write, search, and command tools.

```sh
web2codex web tunnel status
web2codex web tunnel start
web2codex web tunnel stop
web2codex web tunnel remove
```

The API key is not put in the tunnel client's environment. A short-lived private file is used during startup and removed after the tunnel is ready. File tools are scoped to the workspace selected when the tunnel is configured; command tools are launched through Codex's platform sandbox. ChatGPT reports MCP tool discovery after its first connector request.

## Build from source

Install Rust 1.95.0 and platform build tools. On Linux, install `build-essential`, `pkg-config`, GTK 3, WebKitGTK 4.1, and `libxdo`; on macOS, install Xcode Command Line Tools; on Windows, use the MSVC Rust toolchain and Visual Studio C++ Build Tools. Running the browser also requires the operating-system WebView runtime listed above.

From the repository root:

```sh
cargo build --locked --release --manifest-path codex-rs/Cargo.toml -p codex-cli --bin web2codex
cargo build --locked --release --manifest-path codex-rs/Cargo.toml -p codex-web-browser --bin codex-web-browser
```

To publish a release, push a `codex-web-vX.Y.Z` tag. GitHub Actions packages Linux x64/ARM64, Windows x64/ARM64, and macOS Intel/Apple Silicon builds, then publishes installers and checksums. The release workflow also configures the updater to use this repository.

## Project background

Web2Codex is based on the Apache-2.0-licensed [OpenAI Codex repository](https://github.com/openai/codex). See the [Codex documentation](https://developers.openai.com/codex) for standard terminal-agent usage. The private ChatGPT Web window is an independent feature and does not share conversations with the terminal agent.
