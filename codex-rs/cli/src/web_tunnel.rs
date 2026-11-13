//! Background runtime for the Web2Codex MCP connector.

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

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_SUCCESS, FILETIME, GetLastError, HLOCAL, LocalFree,
};
#[cfg(windows)]
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetTokenInformation, PROTECTED_DACL_SECURITY_INFORMATION,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, GetProcessTimes, OpenProcess, OpenProcessToken,
    PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::web_environment::safe_subprocess_environment;

const RELEASE_API: &str = "https://api.github.com/repos/openai/tunnel-client/releases/latest";
const MAX_RELEASE_JSON: usize = 2 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
const MAX_API_KEY_BYTES: usize = 16 * 1024;
const CONNECTOR_NAME: &str = "Web2Codex";

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
    process_start: PathBuf,
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
    anyhow::ensure!(
        !api_key.is_empty(),
        "OpenAI tunnel API key must not be empty"
    );
    anyhow::ensure!(
        api_key.len() <= MAX_API_KEY_BYTES,
        "OpenAI tunnel API key exceeds the 16 KiB limit"
    );
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
        remove_file_if_exists(&paths.api_key)
            .context("failed to remove a stale temporary tunnel API key file")?;
        println!("OpenAI Secure MCP Tunnel is already running for this workspace.");
        print_connector_instructions(tunnel_id);
        print_mcp_observation(&paths.health_url);
        return Ok(());
    }
    if paths.pid.is_file() || runtime_ready() {
        stop()?;
    }

    fs::create_dir_all(&paths.directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&paths.directory, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    set_private_windows_acl(&paths.directory, true)?;
    // Never allow a URL left by a previous run to satisfy this process's readiness check.
    // If it cannot be removed, fail closed instead of starting against stale health state.
    remove_file_if_exists(&paths.health_url)
        .context("failed to remove the previous tunnel health URL")?;
    // A crash during the previous startup can leave the short-lived raw key file behind.
    // No active runtime remains at this point, so clear it before any fallible setup step.
    remove_file_if_exists(&paths.api_key)
        .context("failed to clear a stale temporary tunnel API key file")?;
    let client = install_tunnel_client()?;
    remove_obsolete_client_versions(&paths, &client);
    let codex = std::env::current_exe().context("failed to locate the Codex executable")?;
    let _api_key_cleanup = RemoveFileOnDrop(paths.api_key.clone());
    if let Err(error) = write_private_file(&paths.api_key, api_key.as_bytes()) {
        return Err(error).context("failed to write the temporary tunnel API key file");
    }
    write_profile(&paths, tunnel_id, &paths.api_key, &codex, &workspace)?;

    let log = File::create(&paths.log)
        .with_context(|| format!("failed to open tunnel log {}", paths.log.display()))?;
    let log = Arc::new(Mutex::new(BoundedLog {
        file: log,
        bytes_written: 0,
    }));
    // Verify the executable against its protected digest sidecar immediately before launch.
    // install_tunnel_client also checks the cache, but later file operations leave a TOCTOU gap.
    verify_cached_tunnel_client(&client)?;
    let mut command = Command::new(&client);
    command
        .arg("run")
        .arg("--profile-file")
        .arg(&paths.profile)
        .current_dir(&workspace)
        .env_clear()
        .envs(safe_subprocess_environment())
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
    let Some(process_start) = process_start_token(pid) else {
        let _ = force_terminate_tunnel_process(pid);
        let _ = child.wait();
        remove_runtime_state(&paths)
            .context("failed to clean runtime state after process identity lookup failed")?;
        bail!("could not record the tunnel-client process identity; refusing to start without it");
    };
    if let Err(error) = write_private_file(&paths.pid, pid.to_string().as_bytes())
        .and_then(|()| write_private_file(&paths.process_start, process_start.as_bytes()))
        .and_then(|()| write_private_file(&paths.signature, signature.as_bytes()))
    {
        let _ = terminate_tunnel_process(pid);
        let _ = child.wait();
        return Err(match remove_runtime_state(&paths) {
            Ok(()) => error.context("failed to save tunnel-client process state"),
            Err(cleanup_error) => error.context(format!(
                "failed to save tunnel-client process state; cleanup also failed: {cleanup_error:#}"
            )),
        });
    }
    for _ in 0..150 {
        if runtime_ready() {
            if let Err(error) = fs::remove_file(&paths.api_key) {
                let _ = force_terminate_tunnel_process(pid);
                let _ = child.wait();
                return Err(match remove_runtime_state(&paths) {
                    Ok(()) => anyhow::Error::new(error)
                        .context("failed to remove the temporary tunnel API key file"),
                    Err(cleanup_error) => anyhow::Error::new(error).context(format!(
                        "failed to remove the temporary tunnel API key file; cleanup also failed: {cleanup_error:#}"
                    )),
                });
            }
            remove_obsolete_client_versions(&paths, &client);
            println!("OpenAI Secure MCP Tunnel runtime is ready.");
            print_connector_instructions(tunnel_id);
            print_mcp_observation(&paths.health_url);
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = force_terminate_tunnel_process(pid);
                let _ = child.wait();
                remove_runtime_state(&paths)
                    .context("failed to clean runtime state after tunnel-client exited")?;
                bail!(
                    "tunnel-client exited during startup with {status}; see {}",
                    paths.log.display()
                );
            }
            Ok(None) => {}
            Err(error) => {
                let _ = force_terminate_tunnel_process(pid);
                let _ = child.wait();
                remove_runtime_state(&paths)
                    .context("failed to clean runtime state after a startup monitoring error")?;
                return Err(error).context("failed to monitor tunnel-client startup");
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
    let _ = force_terminate_tunnel_process(pid);
    let _ = child.wait();
    remove_runtime_state(&paths).context("failed to clean runtime state after startup timeout")?;
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
        remove_runtime_state(&paths)?;
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
        remove_runtime_state(&paths)?;
        println!("OpenAI Secure MCP Tunnel is already stopped.");
        return Ok(());
    }

    anyhow::ensure!(
        process_group_exists(pid),
        "a tunnel health listener is responding, but recorded PID {pid} is no longer active; refusing to stop a potentially unrelated process"
    );
    let recorded_start = fs::read_to_string(&paths.process_start).ok();
    let actual_start = process_start_token(pid);
    anyhow::ensure!(
        recorded_start
            .as_deref()
            .is_some_and(|recorded| Some(recorded.trim()) == actual_start.as_deref()),
        "a tunnel health listener is responding, but the recorded process identity is missing or no longer matches PID {pid}; automatic stop is refused to protect unrelated processes. Recovery: use the previous Web2Codex binary to run `web2codex web tunnel stop`, or verify that PID {pid}'s executable path identifies the tunnel-client before ending only that PID"
    );

    terminate_tunnel_process(pid)?;
    for _ in 0..50 {
        if !process_group_exists(pid) && !health_check(&health_url, "/healthz") {
            remove_runtime_state(&paths)?;
            println!("Stopped OpenAI Secure MCP Tunnel.");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    force_terminate_tunnel_process(pid)?;
    for _ in 0..30 {
        if !process_group_exists(pid) && !health_check(&health_url, "/healthz") {
            remove_runtime_state(&paths)?;
            println!("Stopped OpenAI Secure MCP Tunnel.");
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    bail!("tunnel-client process {pid} did not stop; its PID and health records were retained")
}

fn remove_runtime_state(paths: &RuntimePaths) -> anyhow::Result<()> {
    for path in [
        &paths.api_key,
        &paths.pid,
        &paths.process_start,
        &paths.signature,
        &paths.health_url,
    ] {
        remove_file_if_exists(path)
            .with_context(|| format!("failed to remove tunnel state file {}", path.display()))?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
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
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return false;
    }
    let mut exit_code = 0u32;
    let is_running =
        unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0 && exit_code == 259; // STILL_ACTIVE
    unsafe { CloseHandle(process) };
    is_running
}

#[cfg(target_os = "linux")]
fn process_start_token(pid: u32) -> Option<String> {
    // Linux /proc/<pid>/stat field 22 is process start time in clock ticks. The comm
    // field can contain spaces and parentheses, so parse fields after its final ')'.
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let command_end = stat.rfind(')')?;
    stat[command_end + 1..]
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

#[cfg(target_os = "macos")]
fn process_start_token(pid: u32) -> Option<String> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: proc_pidinfo fills a caller-owned proc_bsdinfo buffer of the supplied size.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let expected_size = std::mem::size_of::<libc::proc_bsdinfo>();
    let expected_size_i32 = i32::try_from(expected_size).ok()?;
    let bytes_read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut info as *mut libc::proc_bsdinfo).cast(),
            expected_size_i32,
        )
    };
    (bytes_read == expected_size_i32)
        .then(|| format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec))
}

#[cfg(windows)]
fn process_start_token(pid: u32) -> Option<String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let succeeded =
        unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) } != 0;
    unsafe { CloseHandle(process) };
    succeeded.then(|| format!("{}:{}", creation.dwHighDateTime, creation.dwLowDateTime))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn process_start_token(_pid: u32) -> Option<String> {
    None
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
        .env_clear()
        .envs(safe_subprocess_environment())
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
        .and_then(|paths| {
            let pid = fs::read_to_string(paths.pid)
                .ok()?
                .trim()
                .parse::<u32>()
                .ok()?;
            let expected_start = fs::read_to_string(paths.process_start).ok()?;
            let actual_start = process_start_token(pid)?;
            if actual_start != expected_start.trim() {
                return None;
            }
            let url = fs::read_to_string(paths.health_url).ok()?;
            Some(process_group_exists(pid) && health_check(&url, "/readyz"))
        })
        .unwrap_or(false)
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
        process_start: directory.join("tunnel-client.start"),
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
    #[cfg(windows)]
    set_private_windows_acl(path, false)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(contents)?;
    Ok(())
}

#[cfg(windows)]
fn set_private_windows_acl(path: &Path, inherit_to_children: bool) -> anyhow::Result<()> {
    use std::ptr::null_mut;

    let mut token = null_mut();
    anyhow::ensure!(
        unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } != 0,
        "OpenProcessToken failed: {}",
        unsafe { GetLastError() }
    );
    let mut token_user_size = 0u32;
    unsafe {
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut token_user_size);
    }
    if token_user_size < std::mem::size_of::<TOKEN_USER>() as u32 || token_user_size > 4096 {
        unsafe { CloseHandle(token) };
        anyhow::bail!("invalid current-user token information size");
    }
    let mut token_user_bytes = vec![0u8; token_user_size as usize];
    let token_info_ok = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            token_user_bytes.as_mut_ptr().cast(),
            token_user_size,
            &mut token_user_size,
        )
    };
    unsafe { CloseHandle(token) };
    anyhow::ensure!(
        token_info_ok != 0,
        "GetTokenInformation(TokenUser) failed: {}",
        unsafe { GetLastError() }
    );

    let token_user =
        unsafe { std::ptr::read_unaligned(token_user_bytes.as_ptr().cast::<TOKEN_USER>()) };
    let mut acl = null_mut();
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: 0x001F01FF, // FILE_ALL_ACCESS
        grfAccessMode: SET_ACCESS,
        grfInheritance: if inherit_to_children { 0x03 } else { 0 },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: token_user.User.Sid.cast(),
        },
    };
    let acl_result = unsafe { SetEntriesInAclW(1, &entry, null_mut(), &mut acl) };
    anyhow::ensure!(
        acl_result == ERROR_SUCCESS,
        "SetEntriesInAclW failed: {acl_result}"
    );

    let mut wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let set_result = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_mut_ptr(),
            1, // SE_FILE_OBJECT
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null_mut(),
        )
    };
    unsafe { LocalFree(acl as HLOCAL) };
    anyhow::ensure!(
        set_result == ERROR_SUCCESS,
        "SetNamedSecurityInfoW failed for {}: {}",
        path.display(),
        set_result
    );
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
        .user_agent(concat!("web2codex/", env!("CARGO_PKG_VERSION")))
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
    let digest_path = install_directory.join(format!("{executable_name}.sha256"));
    fs::create_dir_all(&install_directory)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&install_directory, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    set_private_windows_acl(&install_directory, true)?;
    if executable.is_file() {
        #[cfg(windows)]
        {
            set_private_windows_acl(&executable, false)?;
            if digest_path.is_file() {
                set_private_windows_acl(&digest_path, false)?;
            }
        }
        let recorded_digest = fs::read_to_string(&digest_path).unwrap_or_default();
        if recorded_digest
            .trim()
            .eq_ignore_ascii_case(&sha256_file(&executable).unwrap_or_default())
            && !recorded_digest.trim().is_empty()
        {
            return Ok(executable);
        }
        remove_file_if_exists(&executable)?;
        remove_file_if_exists(&digest_path)?;
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
    let executable_digest = format!("{:x}", Sha256::digest(&executable_bytes));
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
    write_private_file(&digest_path, executable_digest.as_bytes())?;
    Ok(executable)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let size = fs::metadata(path)
        .with_context(|| {
            format!(
                "failed to inspect cached tunnel-client at {}",
                path.display()
            )
        })?
        .len();
    anyhow::ensure!(
        size > 0 && size <= MAX_BINARY_BYTES,
        "cached tunnel-client binary size is invalid"
    );
    let mut file = File::open(path).with_context(|| {
        format!(
            "failed to verify cached tunnel-client at {}",
            path.display()
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 16 * 1024];
    let mut bytes_read = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(count as u64);
        anyhow::ensure!(
            bytes_read <= MAX_BINARY_BYTES,
            "cached tunnel-client binary is too large"
        );
        hasher.update(&buffer[..count]);
    }
    anyhow::ensure!(
        bytes_read == size,
        "cached tunnel-client binary changed while hashing"
    );
    Ok(format!("{:x}", hasher.finalize()))
}

fn verify_cached_tunnel_client(executable: &Path) -> anyhow::Result<()> {
    let file_name = executable
        .file_name()
        .context("cached tunnel-client path has no executable name")?;
    let mut digest_name = file_name.to_os_string();
    digest_name.push(".sha256");
    let digest_path = executable.with_file_name(digest_name);
    let expected = fs::read_to_string(&digest_path).with_context(|| {
        format!(
            "failed to read cached tunnel-client digest at {}",
            digest_path.display()
        )
    })?;
    let expected = expected.trim();
    anyhow::ensure!(
        expected.len() == 64 && expected.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "cached tunnel-client digest is invalid"
    );
    let actual = sha256_file(executable)?;
    anyhow::ensure!(
        actual.eq_ignore_ascii_case(expected),
        "cached tunnel-client SHA-256 verification failed"
    );
    Ok(())
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
