//! MCP tools exposed to ChatGPT Web through the configured secure tunnel.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use crate::web_environment::safe_subprocess_environment;
use anyhow::Context;
use codex_mcp::SandboxState;
use codex_protocol::models::PermissionProfile;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;
use rmcp::ErrorData;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::model::CallToolRequestParams;
use rmcp::model::CallToolResponse;
use rmcp::model::CallToolResult;
use rmcp::model::ContentBlock;
use rmcp::model::Implementation;
use rmcp::model::ListToolsResult;
use rmcp::model::PaginatedRequestParams;
use rmcp::model::ServerCapabilities;
use rmcp::model::ServerConfig;
use rmcp::model::Tool;
use rmcp::service::RequestContext;
use rmcp::service::RoleServer;
use tokio::io::AsyncRead;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_SEARCH_MATCHES: usize = 200;
const MAX_SEARCH_FILES: usize = 20_000;
const MAX_DIRECTORY_ENTRIES: usize = 2_000;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) fn run(workspace: PathBuf) -> anyhow::Result<()> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("workspace does not exist: {}", workspace.display()))?;
    anyhow::ensure!(workspace.is_dir(), "workspace is not a directory");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("failed to initialize MCP runtime")?;
    runtime.block_on(async move {
        let service = WebMcpServer { workspace }
            .serve(rmcp::transport::stdio())
            .await
            .context("failed to start Web2Codex MCP server")?;
        service
            .waiting()
            .await
            .context("Web2Codex MCP server stopped")?;
        Ok(())
    })
}

struct WebMcpServer {
    workspace: PathBuf,
}

impl ServerHandler for WebMcpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("web2codex", env!("CARGO_PKG_VERSION")))
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        if request.is_some_and(|request| request.cursor.is_some()) {
            return Err(ErrorData::invalid_params(
                "Web2Codex tools do not use pagination cursors",
                None,
            ));
        }
        let schemas = [
            (
                "get_workspace",
                "Return the absolute root directory for this Web2Codex workspace.",
                json_schema(
                    serde_json::json!({"type":"object","properties":{},"additionalProperties":false}),
                ),
            ),
            (
                "list_directory",
                "List files and directories under a path relative to the workspace root.",
                json_schema(
                    serde_json::json!({"type":"object","properties":{"path":{"type":"string","description":"Relative path; use . for the workspace root."}},"required":["path"],"additionalProperties":false}),
                ),
            ),
            (
                "read_file",
                "Read a UTF-8 text file in the workspace. Files larger than 2 MiB are rejected.",
                json_schema(
                    serde_json::json!({"type":"object","properties":{"path":{"type":"string","description":"Path relative to the workspace root."}},"required":["path"],"additionalProperties":false}),
                ),
            ),
            (
                "write_file",
                "Create or replace a UTF-8 text file in the workspace. Parent directories are created when needed.",
                json_schema(
                    serde_json::json!({"type":"object","properties":{"path":{"type":"string","description":"Path relative to the workspace root."},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}),
                ),
            ),
            (
                "search_files",
                "Search UTF-8 files for a literal, case-insensitive text query. Skips .git, target, node_modules, and hidden directories.",
                json_schema(
                    serde_json::json!({"type":"object","properties":{"query":{"type":"string","minLength":1},"path":{"type":"string","description":"Optional relative directory; defaults to the workspace root."}},"required":["query"],"additionalProperties":false}),
                ),
            ),
            (
                "run_command",
                "Run a shell command through Codex's workspace-write sandbox and return bounded stdout/stderr. Only run commands needed for the user's task.",
                json_schema(
                    serde_json::json!({"type":"object","properties":{"command":{"type":"string","minLength":1}},"required":["command"],"additionalProperties":false}),
                ),
            ),
        ];
        let tools = schemas
            .into_iter()
            .map(|(name, description, schema)| Tool::new(name, description, Arc::new(schema)))
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let arguments = request.arguments.unwrap_or_default();
        let result = match request.name.as_ref() {
            "get_workspace" => Ok(self.workspace.display().to_string()),
            "list_directory" => self.list_directory(required_string(&arguments, "path")?),
            "read_file" => self.read_file(required_string(&arguments, "path")?),
            "write_file" => self.write_file(
                required_string(&arguments, "path")?,
                required_string(&arguments, "content")?,
            ),
            "search_files" => {
                let query = required_string(&arguments, "query")?;
                let path = optional_string(&arguments, "path");
                self.search_files(query, path).await
            }
            "run_command" => {
                self.run_command(required_string(&arguments, "command")?)
                    .await
            }
            _ => {
                return Err(ErrorData::invalid_params("Unknown Web2Codex tool", None));
            }
        };
        let response = match result {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(message) => CallToolResult::error(vec![ContentBlock::text(message)]),
        };
        Ok(response.into())
    }
}

impl WebMcpServer {
    fn list_directory(&self, input: &str) -> Result<String, String> {
        let directory = self.checked_existing_path(input)?;
        if !directory.is_dir() {
            return Err(format!("Not a directory: {input}"));
        }
        let mut entries = std::fs::read_dir(&directory)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .map(|entry| {
                let kind = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    "dir"
                } else {
                    "file"
                };
                format!("{kind}\t{}", entry.file_name().to_string_lossy())
            })
            .collect::<Vec<_>>();
        entries.sort_unstable();
        if entries.len() > MAX_DIRECTORY_ENTRIES {
            entries.truncate(MAX_DIRECTORY_ENTRIES);
            entries.push(format!(
                "… directory listing limited to {MAX_DIRECTORY_ENTRIES} entries"
            ));
        }
        Ok(entries.join("\n"))
    }

    fn read_file(&self, input: &str) -> Result<String, String> {
        let path = self.checked_existing_path(input)?;
        let metadata = std::fs::metadata(&path).map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err(format!("Not a regular file: {input}"));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!("File exceeds the 2 MiB read limit: {input}"));
        }
        std::fs::read_to_string(path).map_err(|error| format!("Could not read UTF-8 file: {error}"))
    }

    fn write_file(&self, input: &str, content: &str) -> Result<String, String> {
        if content.len() as u64 > MAX_FILE_BYTES {
            return Err("Content exceeds the 2 MiB write limit".to_string());
        }
        let path = self.checked_new_path(input)?;
        if std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err("Writing through a symbolic link is not allowed".to_string());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            let canonical_parent = parent.canonicalize().map_err(|error| error.to_string())?;
            if !canonical_parent.starts_with(&self.workspace) {
                return Err("Write path escapes the workspace through a symbolic link".to_string());
            }
        }
        std::fs::write(&path, content).map_err(|error| error.to_string())?;
        Ok(format!(
            "Wrote {} bytes to {}",
            content.len(),
            path.display()
        ))
    }

    async fn search_files(&self, query: &str, input: Option<&str>) -> Result<String, String> {
        if query.is_empty() {
            return Err("Search query must not be empty".to_string());
        }
        let root = match input {
            Some(input) => self.checked_existing_path(input)?,
            None => self.workspace.clone(),
        };
        if !root.is_dir() {
            return Err("Search path is not a directory".to_string());
        }
        let query = query.to_lowercase();
        let mut pending = vec![root];
        let mut scanned = 0usize;
        let mut matches = Vec::new();
        while let Some(directory) = pending.pop() {
            let entries = std::fs::read_dir(&directory).map_err(|error| error.to_string())?;
            for entry in entries.flatten() {
                let path = entry.path();
                let kind = entry.file_type().map_err(|error| error.to_string())?;
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !name.starts_with('.')
                        && !matches!(name.as_str(), "target" | "node_modules" | "vendor")
                    {
                        pending.push(path);
                    }
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                scanned += 1;
                if scanned > MAX_SEARCH_FILES {
                    return Ok(format!(
                        "Search stopped after {MAX_SEARCH_FILES} files. Narrow the path or query.\n{}",
                        matches.join("\n")
                    ));
                }
                if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > MAX_FILE_BYTES) {
                    continue;
                }
                let Ok(contents) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for (line_index, line) in contents.lines().enumerate() {
                    if line.to_lowercase().contains(&query) {
                        let relative = path.strip_prefix(&self.workspace).unwrap_or(&path);
                        matches.push(format!(
                            "{}:{}: {}",
                            relative.display(),
                            line_index + 1,
                            line
                        ));
                        if matches.len() >= MAX_SEARCH_MATCHES {
                            return Ok(matches.join("\n"));
                        }
                    }
                }
            }
        }
        if matches.is_empty() {
            Ok(format!("No matches in {scanned} files."))
        } else {
            Ok(matches.join("\n"))
        }
    }

    async fn run_command(&self, command: &str) -> Result<String, String> {
        if command.trim().is_empty() {
            return Err("Command must not be empty".to_string());
        }

        let command_temp_dir = tempfile::Builder::new()
            .prefix("web2codex-mcp-")
            .tempdir()
            .map_err(|error| {
                format!("Could not create a private command temp directory: {error}")
            })?;
        let temp_root =
            AbsolutePathBuf::from_absolute_path(command_temp_dir.path()).map_err(|error| {
                format!("Could not resolve the private command temp directory: {error}")
            })?;
        let workspace_write_profile = PermissionProfile::workspace_write_with(
            &[temp_root],
            NetworkSandboxPolicy::Restricted,
            /*exclude_tmpdir_env_var*/ true,
            /*exclude_slash_tmp*/ true,
        );
        let mut file_system = workspace_write_profile.file_system_sandbox_policy();
        let root_read_entries = file_system
            .entries
            .iter_mut()
            .filter(|entry| {
                matches!(
                    &entry.path,
                    FileSystemPath::Special {
                        value: FileSystemSpecialPath::Root
                    }
                ) && entry.access == FileSystemAccessMode::Read
            })
            .collect::<Vec<_>>();
        if root_read_entries.len() != 1 {
            return Err("Could not prepare the restricted MCP command sandbox".to_string());
        }
        root_read_entries.into_iter().for_each(|entry| {
            entry.path = FileSystemPath::Special {
                value: FileSystemSpecialPath::Minimal,
            };
        });
        let permission_profile = PermissionProfile::from_runtime_permissions(
            &file_system,
            NetworkSandboxPolicy::Restricted,
        );
        let sandbox_state = SandboxState {
            permission_profile,
            codex_linux_sandbox_exe: None,
            sandbox_cwd: PathUri::from_abs_path(
                &AbsolutePathBuf::from_absolute_path(&self.workspace)
                    .map_err(|error| format!("Could not resolve the workspace path: {error}"))?,
            ),
            use_legacy_landlock: false,
            use_mxc: false,
        };
        let sandbox_state = serde_json::to_string(&sandbox_state)
            .map_err(|error| format!("Could not prepare the workspace sandbox: {error}"))?;
        let codex = std::env::current_exe()
            .map_err(|error| format!("Could not locate the Codex executable: {error}"))?;

        // Re-exec through the same platform-specific sandbox launcher used by Codex.
        // Never fall back to running an MCP-provided shell command unsandboxed.
        let mut process = Command::new(codex);
        process
            .args(["sandbox", "--sandbox-state-json", &sandbox_state, "--"])
            .current_dir(&self.workspace)
            .env_clear()
            .envs(safe_subprocess_environment())
            .env("TMPDIR", command_temp_dir.path())
            .env("TEMP", command_temp_dir.path())
            .env("TMP", command_temp_dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        process.args(["cmd.exe", "/D", "/S", "/C", command]);
        #[cfg(not(windows))]
        process.args(["/bin/sh", "-c", command]);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            process.process_group(0);
        }
        let mut child = process.spawn().map_err(|error| error.to_string())?;
        let stdout = child.stdout.take().ok_or("stdout pipe was not created")?;
        let stderr = child.stderr.take().ok_or("stderr pipe was not created")?;
        let stdout_task = tokio::spawn(read_bounded(stdout, MAX_OUTPUT_BYTES));
        let stderr_task = tokio::spawn(read_bounded(stderr, MAX_OUTPUT_BYTES));
        let status = match timeout(COMMAND_TIMEOUT, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(error.to_string()),
            Err(_) => {
                #[cfg(unix)]
                if let Some(process_id) = child.id() {
                    let _ = codex_utils_pty::process_group::kill_process_group(process_id);
                }
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err("Command exceeded the 5 minute time limit and was stopped".to_string());
            }
        };
        let stdout = stdout_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        let stderr = stderr_task
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
        Ok(format!(
            "exit_code: {}\nstdout:\n{}\nstderr:\n{}",
            status
                .code()
                .map_or_else(|| "terminated".to_string(), |code| code.to_string()),
            String::from_utf8_lossy(&stdout),
            String::from_utf8_lossy(&stderr),
        ))
    }

    fn checked_existing_path(&self, input: &str) -> Result<PathBuf, String> {
        let candidate = self.relative_path(input)?;
        let canonical = candidate
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !canonical.starts_with(&self.workspace) {
            return Err("Path escapes the workspace".to_string());
        }
        Ok(canonical)
    }

    fn checked_new_path(&self, input: &str) -> Result<PathBuf, String> {
        let candidate = self.relative_path(input)?;
        let parent = candidate.parent().ok_or("Path has no parent directory")?;
        let file_name = candidate.file_name().ok_or("Path has no file name")?;
        let mut existing = self.workspace.clone();
        for component in parent.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            existing.push(part);
            match std::fs::symlink_metadata(&existing) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() {
                        return Err("Write path may not traverse a symbolic link".to_string());
                    }
                    if !metadata.is_dir() {
                        return Err("A parent path component is not a directory".to_string());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            }
        }
        existing.push(file_name);
        if existing.exists() {
            let canonical = existing.canonicalize().map_err(|error| error.to_string())?;
            if !canonical.starts_with(&self.workspace) {
                return Err("Write path escapes the workspace".to_string());
            }
        }
        Ok(existing)
    }

    fn relative_path(&self, input: &str) -> Result<PathBuf, String> {
        let path = Path::new(input);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err("Use a relative path inside the workspace".to_string());
        }
        Ok(self.workspace.join(path))
    }
}

fn json_schema(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn required_string<'a>(
    arguments: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Result<&'a str, ErrorData> {
    arguments
        .get(name)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ErrorData::invalid_params(format!("Missing string argument: {name}"), None))
}

fn optional_string<'a>(
    arguments: &'a serde_json::Map<String, serde_json::Value>,
    name: &str,
) -> Option<&'a str> {
    arguments.get(name).and_then(serde_json::Value::as_str)
}

async fn read_bounded<R>(mut reader: R, limit: usize) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut output = Vec::with_capacity(limit.min(16 * 1024));
    let mut buffer = [0u8; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = limit.saturating_sub(output.len());
        output.extend_from_slice(&buffer[..count.min(remaining)]);
    }
    Ok(output)
}
