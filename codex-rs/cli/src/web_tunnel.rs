//! Background runtime for the Codex Web MCP connector.

use std::fs::{self, File, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, bail};
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;
use zip::ZipArchive;

const RELEASE_API: &str = "https://api.github.com/repos/openai/tunnel-client/releases/latest";
const MAX_RELEASE_JSON: usize = 2 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
const CONNECTOR_NAME: &str = "Codex Web";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
}

struct RuntimePaths {
    directory: PathBuf,
    profile: PathBuf,
    api_key: PathBuf,
    log: PathBuf,
    pid: PathBuf,
    signature: PathBuf,
    health_url: PathBuf,
}

struct BoundedLog {
    file: File,
    bytes_written: u64,
}

struct RemoveFileOnDrop(PathBuf);

impl Drop for RemoveFileOnDrop {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn start(tunnel_id: &str, api_key: &str, workspace: &Path) -> anyhow::Result<()> {
    let workspace = workspace
        .canonicalize()
        .with_context(|| format!("workspace does not exist: {}", workspace.display()))?;
    anyhow::ensure!(workspace.is_dir(), "workspace is not a directory");

    let paths = runtime_paths()?;
    let signature = runtime_signature(tunnel_id, api_key, &workspace);
    if runtime_ready()
        && fs::read_to_string(&paths.signature)
            .is_ok_and(|saved| saved.trim() == signature.as_str())
    {
        println!("OpenAI Secure MCP Tunnel is already running for this workspace.");
        print_connector_instructions(tunnel_id);
        print_mcp_observation(&paths.health_url);
        return Ok(());
    }
    if paths.pid.is_file() || runtime_ready() {
        stop()?;
    }

    fs::create_dir_all(&paths.directory)?;
    let _ = fs::remove_file(&paths.health_url);
    let client = install_tunnel_client()?;
    let codex = std::env::current_exe().context("failed to locate the Codex executable")?;
    if let Err(error) = write_private_file(&paths.api_key, api_key.as_bytes()) {
        let _ = fs::remove_file(&paths.api_key);
        return Err(error).context("failed to write the temporary tunnel API key file");
    }
    let _api_key_cleanup = RemoveFileOnDrop(paths.api_key.clone());
    write_profile(&paths, tunnel_id, &paths.api_key, &codex, &workspace)?;

    let log = File::create(&paths.log)
        .with_context(|| format!("failed to open tunnel log {}", paths.log.display()))?;
    let log = Arc::new(Mutex::new(BoundedLog {
        file: log,
        bytes_written: 0,
    }));
    let mut command = Command::new(&client);
    command
        .arg("run")
        .arg("--profile-file")
        .arg(&paths.profile)
        .current_dir(&workspace)
        .env_remove("CONTROL_PLANE_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_file(&paths.api_key);
            return Err(error).with_context(|| {
                format!(
                    "failed to start the OpenAI tunnel client at {}",
                    client.display()
                )
            });
        }
    };
    if let Some(stdout) = child.stdout.take() {
        capture_bounded_log(stdout, Arc::clone(&log));
    }
    if let Some(stderr) = child.stderr.take() {
        capture_bounded_log(stderr, Arc::clone(&log));
    }
    let pid = child.id();
    if let Err(error) = write_private_file(&paths.pid, pid.to_string().as_bytes())
        .and_then(|()| write_private_file(&paths.signature, signature.as_bytes()))
    {
        let _ = terminate_tunnel_process(pid);
        let _ = child.wait();
        remove_runtime_state(&paths);
        return Err(error).context("failed to save tunnel-client process state");
    }
    for _ in 0..150 {
        if runtime_ready() {
            if let Err(error) = fs::remove_file(&paths.api_key) {
                let _ = force_terminate_tunnel_process(pid);
                let _ = child.wait();
                remove_runtime_state(&paths);
                return Err(error).context("failed to remove the temporary tunnel API key file");
            }
            remove_obsolete_client_versions(&paths, &client);
            println!("OpenAI Secure MCP Tunnel runtime is ready.");
            print_connector_instructions(tunnel_id);
            print_mcp_observation(&paths.health_url);
            return Ok(());
        }
        if let Some(status) = child.try_wait()? {
            let _ = force_terminate_tunnel_process(pid);
            remove_runtime_state(&paths);
            bail!(
                "tunnel-client exited during startup with {status}; see {}",
                paths.log.display()
            );
        }
        thread::sleep(Duration::from_millis(200));
    }
    let _ = force_terminate_tunnel_process(pid);
    let _ = child.wait();
    remove_runtime_state(&paths);
    bail!(
        "tunnel-client did not become ready within 30 seconds; see {}",
        paths.log.display()
    );
}

pub(crate) fn status() -> anyhow::Result<()> {
    if runtime_ready() {
        println!("OpenAI Secure MCP Tunnel runtime is ready.");
        println!("Connector name: {CONNECTOR_NAME}");
        let paths = runtime_paths()?;
        print_mcp_observation(&paths.health_url);
    } else {
        println!("OpenAI Secure MCP Tunnel is stopped or not ready yet.");
        let paths = runtime_paths()?;
        if paths.log.is_file() {
            println!("Log: {}", paths.log.display());
        }
    }
    Ok(())
}

pub(crate) fn stop() -> anyhow::Result<()> {
    let paths = runtime_paths()?;
    let pid = match fs::read_to_string(&paths.pid) {
        Ok(value) => value.trim().parse::<u32>().ok(),
        Err(_) => None,
    };
    let Some(pid) = pid else {
        let health_url = fs::read_to_string(&paths.health_url).unwrap_or_default();
        anyhow::ensure!(
            !health_check(&health_url, "/healthz"),
            "a tunnel runtime is responding but its PID record is missing; refusing to start or stop an unmanaged process"
        );
        remove_runtime_state(&paths);
        println!("OpenAI Secure MCP Tunnel is already stopped.");
        return Ok(());
    };

    let health_url = fs::read_to_string(&paths.health_url).unwrap_or_default();
    if !health_check(&health_url, "/healthz") {
        if process_group_exists(pid) {
            bail!(
                "tunnel-client process {pid} is still running but its health listener is unavailable; state files were kept to avoid stopping an unrelated process"
            );
        }
        remove_runtime_state(&paths);
        println!("OpenAI Secure MCP Tunnel is already stopped.");
        return Ok(());
    }

    terminate_tunnel_process(pid)?;
    for _ in 0..50 {
        if !process_group_exists(pid) && !health_check(&health_url, "/healthz") {
            remove_runtime_state(&paths);
            println!("Stopped OpenAI Secure MCP Tunnel.");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    force_terminate_tunnel_process(pid)?;
    for _ in 0..30 {
        if !process_group_exists(pid) && !health_check(&health_url, "/healthz") {
            remove_runtime_state(&paths);
            println!("Stopped OpenAI Secure MCP Tunnel.");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("tunnel-client process {pid} did not stop; its PID and health records were retained")
}

fn remove_runtime_state(paths: &RuntimePaths) {
    let _ = fs::remove_file(&paths.pid);
    let _ = fs::remove_file(&paths.signature);
    let _ = fs::remove_file(&paths.health_url);
    let _ = fs::remove_file(&paths.api_key);
}

#[cfg(unix)]
fn process_group_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal zero checks whether the process group exists without sending a signal.
    let result = unsafe { libc::kill(-pid, 0) };
    result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_group_exists(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    let Ok(output) = Command::new("tasklist")
        .args(["/FI", filter.as_str(), "/FO", "CSV", "/NH"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let pid_field = format!(",\"{pid}\",");
    String::from_utf8_lossy(&output.stdout).contains(pid_field.as_str())
}

#[cfg(unix)]
fn terminate_tunnel_process(pid: u32) -> anyhow::Result<()> {
    let pid = libc::pid_t::try_from(pid).context("tunnel-client PID is out of range")?;
    // SAFETY: the process was launched in its own process group by this module.
    let result = unsafe { libc::kill(-pid, libc::SIGTERM) };
    anyhow::ensure!(
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "failed to send SIGTERM to tunnel-client process group {pid}"
    );
    Ok(())
}

#[cfg(windows)]
fn terminate_tunnel_process(pid: u32) -> anyhow::Result<()> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to stop tunnel-client")?;
    anyhow::ensure!(
        status.success(),
        "could not stop tunnel-client process {pid}"
    );
    Ok(())
}

#[cfg(unix)]
fn force_terminate_tunnel_process(pid: u32) -> anyhow::Result<()> {
    let pid = libc::pid_t::try_from(pid).context("tunnel-client PID is out of range")?;
    // SAFETY: the process was launched in its own process group by this module.
    let result = unsafe { libc::kill(-pid, libc::SIGKILL) };
    anyhow::ensure!(
        result == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH),
        "failed to send SIGKILL to tunnel-client process group {pid}"
    );
    Ok(())
}

#[cfg(windows)]
fn force_terminate_tunnel_process(pid: u32) -> anyhow::Result<()> {
    terminate_tunnel_process(pid)
}

fn print_connector_instructions(tunnel_id: &str) {
    println!("In ChatGPT, add an MCP connector named `{CONNECTOR_NAME}` using Connection: Tunnel.");
    println!("Tunnel ID: {tunnel_id}");
}

fn print_mcp_observation(health_url_file: &Path) {
    let health_url = fs::read_to_string(health_url_file).unwrap_or_default();
    let Some((200, body)) = health_response(&health_url, "/health/mcp") else {
        println!(
            "MCP discovery has not been observed yet; ChatGPT will initialize it on its first connector request."
        );
        return;
    };
    let Ok(health) = serde_json::from_slice::<serde_json::Value>(&body) else {
        println!("MCP discovery state is unavailable; the tunnel runtime itself is ready.");
        return;
    };
    let state = health
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let status = health
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let tools = health
        .pointer("/details/tools_list/tool_names")
        .and_then(serde_json::Value::as_array);
    let complete = health
        .pointer("/details/tools_list/complete")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if state == "discovered" && status == "ok" && complete {
        let names = tools
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>();
        println!("MCP server discovered; exposed tools: {}", names.join(", "));
    } else {
        println!(
            "MCP discovery state: {state}; it will be observed after ChatGPT initializes the connector."
        );
    }
}

fn runtime_ready() -> bool {
    runtime_paths()
        .ok()
        .and_then(|paths| fs::read_to_string(paths.health_url).ok())
        .is_some_and(|url| health_check(&url, "/readyz"))
}

fn runtime_paths() -> anyhow::Result<RuntimePaths> {
    let directory = codex_core::config::find_codex_home()
        .context("failed to resolve CODEX_HOME")?
        .join("web-tunnel")
        .to_path_buf();
    Ok(RuntimePaths {
        profile: directory.join("profile.yaml"),
        api_key: directory.join("runtime-api-key.tmp"),
        log: directory.join("tunnel-client.log"),
        pid: directory.join("tunnel-client.pid"),
        signature: directory.join("runtime.signature"),
        health_url: directory.join("health-url.txt"),
        directory,
    })
}

fn runtime_signature(tunnel_id: &str, api_key: &str, workspace: &Path) -> String {
    let workspace = workspace.to_string_lossy();
    let mut hasher = Sha256::new();
    for value in [
        tunnel_id.as_bytes(),
        api_key.as_bytes(),
        workspace.as_bytes(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value);
    }
    format!("{:x}", hasher.finalize())
}

fn capture_bounded_log<R>(mut reader: R, log: Arc<Mutex<BoundedLog>>)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let Ok(mut log) = log.lock() else {
                break;
            };
            let remaining = MAX_LOG_BYTES.saturating_sub(log.bytes_written) as usize;
            let count = count.min(remaining);
            if count == 0 {
                continue;
            }
            if log.file.write_all(&buffer[..count]).is_err() {
                break;
            }
            log.bytes_written += count as u64;
        }
    });
}

fn write_profile(
    paths: &RuntimePaths,
    tunnel_id: &str,
    api_key_file: &Path,
    codex: &Path,
    workspace: &Path,
) -> anyhow::Result<()> {
    let command = format!(
        "{} web mcp-server --workspace {}",
        quote_argument(codex),
        quote_argument(workspace)
    );
    let profile = format!(
        "config_version: 1\n\
         control_plane:\n\
           tunnel_id: {}\n\
           api_key: {}\n\
         health:\n\
           listen_addr: {}\n\
           url_file: {}\n\
         admin_ui:\n\
           open_browser: false\n\
         log:\n\
           level: info\n\
           format: json\n\
         mcp:\n\
           commands:\n\
           - channel: main\n\
             command: {}\n",
        yaml_string(tunnel_id),
        yaml_string(&format!("file:{}", api_key_file.to_string_lossy())),
        yaml_string("127.0.0.1:0"),
        yaml_string(&paths.health_url.to_string_lossy()),
        yaml_string(&command),
    );
    write_private_file(&paths.profile, profile.as_bytes())
        .context("failed to write the tunnel-client profile")
}

fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializing a string to JSON cannot fail")
}

fn quote_argument(path: &Path) -> String {
    let mut value = path.to_string_lossy().replace('"', "\\\"");
    #[cfg(windows)]
    {
        value = value.replace('\\', "/");
    }
    format!("\"{value}\"")
}

fn write_private_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(contents)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn health_check(base_url: &str, endpoint: &str) -> bool {
    health_response(base_url, endpoint).is_some_and(|(status, _)| status == 200)
}

fn health_response(base_url: &str, endpoint: &str) -> Option<(u16, Vec<u8>)> {
    let Ok(mut url) = Url::parse(base_url.trim()) else {
        return None;
    };
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return None;
    }
    let Some(host) = url.host_str() else {
        return None;
    };
    let Ok(ip) = host.parse::<std::net::IpAddr>() else {
        return None;
    };
    if !ip.is_loopback() {
        return None;
    }
    let Some(port) = url.port_or_known_default() else {
        return None;
    };
    let address = SocketAddr::new(ip, port);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(300)) else {
        return None;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    url.set_path(endpoint);
    url.set_query(None);
    let Some(host_header) = url.host_str() else {
        return None;
    };
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        url.path(),
        host_header
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return None;
    }
    const MAX_HEALTH_RESPONSE_BYTES: u64 = 64 * 1024;
    let mut response = Vec::new();
    if stream
        .take(MAX_HEALTH_RESPONSE_BYTES + 1)
        .read_to_end(&mut response)
        .is_err()
        || response.len() as u64 > MAX_HEALTH_RESPONSE_BYTES
    {
        return None;
    }
    let separator = response.windows(4).position(|bytes| bytes == b"\r\n\r\n")?;
    let headers = std::str::from_utf8(&response[..separator]).ok()?;
    let status = headers
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, response[separator + 4..].to_vec()))
}

fn install_tunnel_client() -> anyhow::Result<PathBuf> {
    let (os, arch) = target_asset_parts()?;
    let client = Client::builder()
        .user_agent(concat!("codex-web/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(90))
        .build()
        .context("failed to initialize the tunnel-client downloader")?;
    let response = client
        .get(RELEASE_API)
        .send()
        .context("failed to look up the latest OpenAI tunnel-client release")?;
    anyhow::ensure!(
        response.status().is_success(),
        "GitHub release lookup failed: {}",
        response.status()
    );
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RELEASE_JSON as u64)
    {
        bail!("GitHub release metadata is too large");
    }
    let mut release_json = Vec::new();
    response
        .take(MAX_RELEASE_JSON as u64 + 1)
        .read_to_end(&mut release_json)?;
    anyhow::ensure!(
        release_json.len() <= MAX_RELEASE_JSON,
        "GitHub release metadata is too large"
    );
    let release: Release = serde_json::from_slice(&release_json)
        .context("GitHub returned invalid tunnel-client release metadata")?;
    anyhow::ensure!(
        release
            .tag_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
        "GitHub release tag contains unexpected characters"
    );
    let archive_name = format!("tunnel-client-{}-{os}-{arch}.zip", release.tag_name);
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == archive_name)
        .with_context(|| format!("the latest tunnel-client release has no {os}/{arch} archive"))?;
    let expected_sha256 = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .context("GitHub did not provide a SHA-256 digest for the tunnel-client archive")?;
    anyhow::ensure!(
        asset
            .browser_download_url
            .starts_with("https://github.com/openai/tunnel-client/releases/download/"),
        "GitHub returned an unexpected tunnel-client download URL"
    );
    let executable_name = if cfg!(windows) {
        "tunnel-client.exe"
    } else {
        "tunnel-client"
    };
    let paths = runtime_paths()?;
    let install_directory = paths.directory.join("bin").join(&release.tag_name);
    let executable = install_directory.join(executable_name);
    if executable.is_file() {
        return Ok(executable);
    }
    let response = client
        .get(&asset.browser_download_url)
        .send()
        .context("failed to download tunnel-client")?;
    anyhow::ensure!(
        response.status().is_success(),
        "tunnel-client download failed: {}",
        response.status()
    );
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES as u64)
    {
        bail!("tunnel-client archive exceeds the 128 MiB download limit");
    }
    let mut archive_bytes = Vec::new();
    response
        .take(MAX_ARCHIVE_BYTES as u64 + 1)
        .read_to_end(&mut archive_bytes)?;
    anyhow::ensure!(
        archive_bytes.len() <= MAX_ARCHIVE_BYTES,
        "tunnel-client archive exceeds the 128 MiB download limit"
    );
    let actual_sha256 = format!("{:x}", Sha256::digest(&archive_bytes));
    anyhow::ensure!(
        actual_sha256.eq_ignore_ascii_case(expected_sha256),
        "SHA-256 verification failed for tunnel-client archive"
    );

    let mut archive = ZipArchive::new(Cursor::new(archive_bytes))
        .context("tunnel-client release is not a valid ZIP archive")?;
    let mut executable_bytes = None;
    for index in 0..archive.len() {
        let entry = archive.by_index(index)?;
        let entry_name = entry.name()?;
        let is_target = Path::new(entry_name.as_ref())
            .file_name()
            .is_some_and(|name| name == executable_name);
        let is_symlink = entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000);
        if !is_target || entry.is_dir() || is_symlink {
            continue;
        }
        let size = entry.size();
        anyhow::ensure!(
            size > 0 && size <= MAX_BINARY_BYTES,
            "tunnel-client binary size is invalid"
        );
        let mut bytes = Vec::with_capacity(size as usize);
        entry.take(MAX_BINARY_BYTES + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            bytes.len() as u64 == size,
            "tunnel-client binary was truncated"
        );
        executable_bytes = Some(bytes);
        break;
    }
    let executable_bytes =
        executable_bytes.context("tunnel-client executable was missing from the release ZIP")?;
    fs::create_dir_all(&install_directory)?;
    let temporary = install_directory.join(format!("{executable_name}.partial"));
    write_private_file(&temporary, &executable_bytes)?;
    fs::rename(&temporary, &executable).with_context(|| {
        format!(
            "failed to install tunnel-client at {}",
            executable.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    }
    Ok(executable)
}

fn target_asset_parts() -> anyhow::Result<(&'static str, &'static str)> {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        bail!("OpenAI tunnel-client is not available for this operating system")
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        _ => bail!("OpenAI tunnel-client is not available for this CPU architecture"),
    };
    Ok((os, arch))
}

fn remove_obsolete_client_versions(paths: &RuntimePaths, current_client: &Path) {
    let bin_directory = paths.directory.join("bin");
    let Some(current_directory) = current_client.parent() else {
        return;
    };
    let Ok(entries) = fs::read_dir(bin_directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path != current_directory && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = fs::remove_dir_all(path);
        }
    }
}
