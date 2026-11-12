# Codex CLI - community fork

This repository is a community fork of [OpenAI Codex CLI](https://github.com/openai/codex). It keeps the CLI's existing model, authentication, tool, and native subagent behavior, and adds an optional private ChatGPT Web window built with each operating system's native WebView.

The browser is independent of CLI conversations and tasks. It has its own persistent sign-in profile, does not pass browser conversations into Codex, and does not start with Codex or when the computer starts.

## Install this fork

Use the platform installer attached to the fork's latest GitHub Release. Replace `OWNER/REPOSITORY` with the repository's GitHub path:

```sh
curl -fsSL https://github.com/OWNER/REPOSITORY/releases/latest/download/install.sh | sh
```

```powershell
irm https://github.com/OWNER/REPOSITORY/releases/latest/download/install.ps1 | iex
```

The release installers verify package SHA-256 digests. Run `codex` to start the CLI and follow its sign-in instructions. The CLI uses Codex's existing ChatGPT-plan or API-key authentication; the browser's ChatGPT Web sign-in is separate.

## Open ChatGPT Web

```text
codex web          # Open the private browser window
codex web open
codex web login    # Open ChatGPT so you can sign in to its separate profile
codex web setup
codex web start    # Start an idle hidden process; ChatGPT is not loaded
codex web hide     # Hide the window and release the WebView's memory
codex web status
codex web stop
```

The WebView is created only after an explicit open, login, or setup command. Hiding or closing the window releases the WebView while keeping the lightweight controller alive; the separate profile preserves sign-in data. Use `codex web stop` to exit the controller. It never launches automatically.

Windows uses the Microsoft Edge WebView2 Runtime, which is included with Windows 11. Other Windows systems can install it using [Microsoft's WebView2 deployment guide](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution). macOS uses the built-in WKWebView. On Linux, the CLI is built for musl while the browser sidecar uses the system GNU/WebKitGTK 4.1 stack; opening the browser requires glibc, GTK 3, and WebKitGTK 4.1 runtime libraries. These Linux libraries are not bundled with the release.

## Build from source

Install Rust 1.95.0 and the platform build tools, then build the CLI and browser. On Debian or Ubuntu, install `build-essential`, `pkg-config`, `libgtk-3-dev`, `libwebkit2gtk-4.1-dev`, and `libxdo-dev`. On macOS, install the Xcode Command Line Tools. On Windows, use the MSVC Rust toolchain with Visual Studio C++ Build Tools. The WebView2 runtime is needed to run the browser on Windows.

For a local build, set `CODEX_GITHUB_REPOSITORY` to this fork so update links point to its releases:

```sh
export CODEX_GITHUB_REPOSITORY=OWNER/REPOSITORY
```

```powershell
$env:CODEX_GITHUB_REPOSITORY = 'OWNER/REPOSITORY'
```

Then build both binaries:

```sh
cargo build --release --manifest-path codex-rs/Cargo.toml -p codex-cli --bin codex
cargo build --release --manifest-path codex-rs/Cargo.toml -p codex-web-browser --bin codex-web-browser
```

For a fork release, push a `codex-web-vX.Y.Z` tag. GitHub Actions builds CLI and browser packages for Linux x64 and ARM64, Windows x64 and ARM64, and macOS Intel and Apple Silicon, then publishes the installers and checksum manifest. Standalone `codex update` reads releases from the repository where the binary was built; npm and Homebrew installs retain their existing package-manager-specific update path.

## Codex CLI documentation

The CLI source and existing behavior are retained from OpenAI's project. The upstream documentation covers standard CLI usage and development:

- [Codex documentation](https://developers.openai.com/codex)
- [Contributing](./docs/contributing.md)
- [Installing and building](./docs/install.md)
- [Open source fund](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
