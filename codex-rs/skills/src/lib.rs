mod interface;
mod invocation;
mod loading;
mod mentions;
mod metadata;
mod model;
mod name_counts;
mod parser;
mod selection;

pub use interface::SkillInterfaceAssetPolicy;
pub use interface::SkillInterfaceFile;
pub use interface::resolve_skill_interface;
pub use invocation::ImplicitSkillAccess;
pub use invocation::ImplicitSkillLookup;
pub use invocation::detect_implicit_skill_invocation_for_command;
pub use invocation::implicit_skill_accesses_for_command;
pub use loading::LoadedSkillRoot;
pub use loading::LoadedSkills;
pub use loading::SkillError;
pub use loading::SkillLoadFuture;
pub use loading::SkillRootLoadRequest;
pub use loading::SkillRootLoader;
pub use loading::SkillRootSnapshotCache;
pub use loading::SkillRootSnapshots;
pub use mentions::ToolMentionKind;
pub use mentions::ToolMentions;
pub use mentions::app_id_from_path;
pub use mentions::extract_tool_mentions;
pub use mentions::extract_tool_mentions_with_sigil;
pub use mentions::normalize_skill_path;
pub use mentions::plugin_config_name_from_path;
pub use mentions::tool_kind_for_path;
pub use metadata::ParsedSkillMetadata;
pub use metadata::parse_skill_metadata;
pub use model::EnvironmentSkillMetadata;
pub use model::SkillDependencies;
pub use model::SkillInterface;
pub use model::SkillMetadata;
pub use model::SkillPolicy;
pub use model::SkillToolDependency;
pub use name_counts::build_skill_name_counts;
pub use parser::ParsedSkillFrontmatter;
pub use parser::SkillParseError;
pub use parser::parse_skill_frontmatter_metadata;
pub use selection::ExplicitSkillLookup;
pub use selection::collect_explicit_skill_mentions;

use codex_utils_absolute_path::AbsolutePathBuf;
use include_dir::Dir;
use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::Hash;
use std::hash::Hasher;

use thiserror::Error;

const SYSTEM_SKILLS_DIR: Dir = include_dir::include_dir!("$CARGO_MANIFEST_DIR/src/assets/samples");

const SYSTEM_SKILLS_DIR_NAME: &str = ".system";
const SKILLS_DIR_NAME: &str = "skills";
const SYSTEM_SKILLS_MARKER_FILENAME: &str = ".codex-system-skills.marker";
const SYSTEM_SKILLS_MARKER_SALT: &str = "v1";

/// Returns the on-disk cache location for embedded system skills from an absolute CODEX_HOME.
pub fn system_cache_root_dir(codex_home: &AbsolutePathBuf) -> AbsolutePathBuf {
    codex_home
        .join(SKILLS_DIR_NAME)
        .join(SYSTEM_SKILLS_DIR_NAME)
}

/// Installs embedded system skills into `CODEX_HOME/skills/.system`.
///
/// Clears any existing system skills directory first and then writes the embedded
/// skills directory into place.
///
/// To avoid doing unnecessary work on every startup, a marker file is written
/// with a fingerprint of the embedded directory. When the marker matches, the
/// install is skipped.
pub fn install_system_skills(codex_home: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    let skills_root_dir = codex_home.join(SKILLS_DIR_NAME);
    fs::create_dir_all(skills_root_dir.as_path())
        .map_err(|source| SystemSkillsError::io("create skills root dir", source))?;

    // Several Web2Codex processes can start at once when a request fans out to
    // subagents. Serialize the shared cache replacement across processes so a
    // second startup cannot delete the directory while the first is writing it.
    let install_lock_path = skills_root_dir.join(".system-skills-install.lock");
    let mut _install_lock =
        fslock::LockFile::open(install_lock_path.as_path()).map_err(|source| {
            SystemSkillsError::io(
                "open system skills install lock",
                std::io::Error::other(source),
            )
        })?;
    _install_lock.lock().map_err(|source| {
        SystemSkillsError::io("lock system skills install", std::io::Error::other(source))
    })?;

    let dest_system = system_cache_root_dir(codex_home);

    let marker_path = dest_system.join(SYSTEM_SKILLS_MARKER_FILENAME);
    let expected_fingerprint = embedded_system_skills_fingerprint();
    if dest_system.as_path().is_dir()
        && read_marker(&marker_path).is_ok_and(|marker| marker == expected_fingerprint)
    {
        return Ok(());
    }

    if dest_system.as_path().exists() {
        fs::remove_dir_all(dest_system.as_path())
            .map_err(|source| SystemSkillsError::io("remove existing system skills dir", source))?;
    }

    write_embedded_dir(&SYSTEM_SKILLS_DIR, &dest_system)?;
    fs::write(marker_path.as_path(), format!("{expected_fingerprint}\n"))
        .map_err(|source| SystemSkillsError::io("write system skills marker", source))?;
    Ok(())
}

fn read_marker(path: &AbsolutePathBuf) -> Result<String, SystemSkillsError> {
    Ok(fs::read_to_string(path.as_path())
        .map_err(|source| SystemSkillsError::io("read system skills marker", source))?
        .trim()
        .to_string())
}

fn embedded_system_skills_fingerprint() -> String {
    let mut items = Vec::new();
    collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
    items.sort_unstable_by(|(a, _), (b, _)| a.cmp(b));

    let mut hasher = DefaultHasher::new();
    SYSTEM_SKILLS_MARKER_SALT.hash(&mut hasher);
    for (path, contents_hash) in items {
        path.hash(&mut hasher);
        contents_hash.hash(&mut hasher);
    }
    format!("{:x}", hasher.finish())
}

fn collect_fingerprint_items(dir: &Dir<'_>, items: &mut Vec<(String, Option<u64>)>) {
    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                items.push((subdir.path().to_string_lossy().to_string(), None));
                collect_fingerprint_items(subdir, items);
            }
            include_dir::DirEntry::File(file) => {
                let mut file_hasher = DefaultHasher::new();
                file.contents().hash(&mut file_hasher);
                items.push((
                    file.path().to_string_lossy().to_string(),
                    Some(file_hasher.finish()),
                ));
            }
        }
    }
}

/// Writes the embedded `include_dir::Dir` to disk under `dest`.
///
/// Preserves the embedded directory structure.
fn write_embedded_dir(dir: &Dir<'_>, dest: &AbsolutePathBuf) -> Result<(), SystemSkillsError> {
    fs::create_dir_all(dest.as_path())
        .map_err(|source| SystemSkillsError::io("create system skills dir", source))?;

    for entry in dir.entries() {
        match entry {
            include_dir::DirEntry::Dir(subdir) => {
                let subdir_dest = dest.join(subdir.path());
                fs::create_dir_all(subdir_dest.as_path()).map_err(|source| {
                    SystemSkillsError::io("create system skills subdir", source)
                })?;
                write_embedded_dir(subdir, dest)?;
            }
            include_dir::DirEntry::File(file) => {
                let path = dest.join(file.path());
                if let Some(parent) = path.as_path().parent() {
                    fs::create_dir_all(parent).map_err(|source| {
                        SystemSkillsError::io("create system skills file parent", source)
                    })?;
                }
                fs::write(path.as_path(), file.contents())
                    .map_err(|source| SystemSkillsError::io("write system skill file", source))?;
            }
        }
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum SystemSkillsError {
    #[error("io error while {action}: {source}")]
    Io {
        action: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl SystemSkillsError {
    fn io(action: &'static str, source: std::io::Error) -> Self {
        Self::Io { action, source }
    }
}

#[cfg(test)]
mod tests {
    use super::SYSTEM_SKILLS_DIR;
    use super::SYSTEM_SKILLS_MARKER_FILENAME;
    use super::collect_fingerprint_items;
    use super::embedded_system_skills_fingerprint;
    use super::install_system_skills;
    use super::read_marker;
    use super::system_cache_root_dir;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use std::fs;
    use std::process::Command;
    use std::process::Stdio;
    use std::sync::atomic::AtomicU64;
    use std::sync::atomic::Ordering;
    use std::thread;

    #[test]
    fn fingerprint_traverses_nested_entries() {
        let mut items = Vec::new();
        collect_fingerprint_items(&SYSTEM_SKILLS_DIR, &mut items);
        let mut paths: Vec<String> = items.into_iter().map(|(path, _)| path).collect();
        paths.sort_unstable();

        assert!(
            paths
                .binary_search_by(|probe| probe.as_str().cmp("skill-creator/SKILL.md"))
                .is_ok()
        );
        assert!(
            paths
                .binary_search_by(|probe| probe.as_str().cmp("skill-creator/scripts/init_skill.py"))
                .is_ok()
        );
    }

    #[test]
    fn concurrent_system_skill_installations_share_a_valid_cache() {
        static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);

        let unique = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let temp_home = std::env::temp_dir().join(format!(
            "codex-skills-concurrent-install-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_home).expect("create temporary CODEX_HOME");
        let codex_home = AbsolutePathBuf::try_from(temp_home.clone())
            .expect("temporary CODEX_HOME path should be absolute");

        let installers = (0..8)
            .map(|_| {
                let codex_home = codex_home.clone();
                thread::spawn(move || install_system_skills(&codex_home))
            })
            .collect::<Vec<_>>();

        for installer in installers {
            installer
                .join()
                .expect("installer thread should not panic")
                .expect("concurrent install should succeed");
        }

        let cache_root = system_cache_root_dir(&codex_home);
        let marker = cache_root.join(SYSTEM_SKILLS_MARKER_FILENAME);
        assert_eq!(
            read_marker(&marker).expect("installed marker should be readable"),
            embedded_system_skills_fingerprint()
        );
        fs::remove_dir_all(&temp_home).expect("remove temporary CODEX_HOME");
    }

    #[test]
    fn concurrent_system_skill_installations_across_processes_share_a_valid_cache() {
        static NEXT_TEMP_DIR: AtomicU64 = AtomicU64::new(0);
        const CHILD_HOME_ENV: &str = "CODEX_SKILLS_INSTALL_TEST_CHILD_HOME";

        let unique = NEXT_TEMP_DIR.fetch_add(1, Ordering::Relaxed);
        let temp_home = std::env::temp_dir().join(format!(
            "codex-skills-process-install-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&temp_home).expect("create temporary CODEX_HOME");

        let executable = std::env::current_exe().expect("locate current test executable");
        let mut children = Vec::new();
        let mut spawn_errors = Vec::new();
        for _ in 0..8 {
            match Command::new(&executable)
                .args(["--exact", "tests::install_system_skills_child_process"])
                .env(CHILD_HOME_ENV, &temp_home)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(child) => children.push(child),
                Err(error) => spawn_errors.push(error.to_string()),
            }
        }

        let statuses = children
            .into_iter()
            .map(|mut child| child.wait())
            .collect::<Vec<_>>();
        let cache_root = temp_home.join("skills").join(".system");
        let marker = cache_root.join(SYSTEM_SKILLS_MARKER_FILENAME);
        let installed_fingerprint = fs::read_to_string(&marker);
        let cleanup_result = fs::remove_dir_all(&temp_home);

        assert!(
            spawn_errors.is_empty(),
            "could not launch child tests: {spawn_errors:?}"
        );
        assert!(
            statuses
                .iter()
                .all(|status| status.as_ref().is_ok_and(|status| status.success())),
            "one or more child installers failed: {statuses:?}"
        );
        assert_eq!(
            installed_fingerprint
                .expect("concurrent child installs should leave a readable marker")
                .trim(),
            embedded_system_skills_fingerprint()
        );
        cleanup_result.expect("remove temporary CODEX_HOME");
    }

    #[test]
    fn install_system_skills_child_process() {
        const CHILD_HOME_ENV: &str = "CODEX_SKILLS_INSTALL_TEST_CHILD_HOME";
        let Some(home) = std::env::var_os(CHILD_HOME_ENV) else {
            return;
        };
        let codex_home = AbsolutePathBuf::try_from(std::path::PathBuf::from(home))
            .expect("child CODEX_HOME is absolute");
        install_system_skills(&codex_home).expect("child process should install system skills");
    }
}
