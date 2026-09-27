use std::thread;
use std::{
    env, fs,
    fs::{File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde_json::{Value, json};

use crate::app::execution::{HistoryKey, SuccessfulTarget};

const HISTORY_VERSION: u64 = 1;
const HISTORY_LOCK_WAIT: Duration = Duration::from_secs(5);
const HISTORY_LOCK_RETRY: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    #[must_use]
    pub(crate) const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub(crate) fn platform() -> Option<Self> {
        platform_directory().map(Self::new)
    }

    pub(crate) fn load_many(&self, keys: &[Option<HistoryKey>]) -> Vec<Option<Duration>> {
        let mut durations = vec![None; keys.len()];
        if !self.root.is_dir() || keys.iter().all(Option::is_none) {
            return durations;
        }
        let Ok(lock) = self.lock_file() else {
            return durations;
        };
        if lock_history(&lock).is_err() {
            return durations;
        }
        for (index, key) in keys.iter().enumerate() {
            if let Some(key) = key {
                durations[index] = read_duration(&self.record_path(key));
            }
        }
        durations
    }

    pub(crate) fn record(&self, successes: &[SuccessfulTarget]) -> io::Result<()> {
        if successes.is_empty() {
            return Ok(());
        }
        fs::create_dir_all(&self.root)?;
        set_directory_permissions(&self.root)?;
        let lock = self.lock_file()?;
        lock_history(&lock)?;

        let recorded_at = unix_millis();
        for success in successes {
            let path = self.record_path(&success.key);
            let document = json!({
                "version": HISTORY_VERSION,
                "duration_ms": duration_millis(success.duration),
                "recorded_at_unix_ms": recorded_at,
            });
            write_atomically(&path, &document)?;
        }
        Ok(())
    }

    fn record_path(&self, key: &HistoryKey) -> PathBuf {
        self.root.join(format!("{}.json", key.file_stem()))
    }

    fn lock_file(&self) -> io::Result<File> {
        let path = self.root.join("history.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options.open(path)?;
        set_file_permissions(&file, 0o600)?;
        Ok(file)
    }
}

fn lock_history(file: &File) -> io::Result<()> {
    let deadline = Instant::now() + HISTORY_LOCK_WAIT;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(HISTORY_LOCK_RETRY);
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for Terraleph history lock",
                ));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
    }
}

fn read_duration(path: &Path) -> Option<Duration> {
    let bytes = fs::read(path).ok()?;
    let value = serde_json::from_slice::<Value>(&bytes).ok()?;
    (value.get("version")?.as_u64()? == HISTORY_VERSION).then_some(())?;
    Some(Duration::from_millis(value.get("duration_ms")?.as_u64()?))
}

fn write_atomically(path: &Path, document: &Value) -> io::Result<()> {
    let sequence = TEMP_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temporary = path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("history"),
        std::process::id(),
        sequence
    ));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary)?;
        set_file_permissions(&file, 0o600)?;
        let mut bytes = serde_json::to_vec(document).map_err(io::Error::other)?;
        bytes.push(b'\n');
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, path: &Path) -> io::Result<()> {
    fs::rename(temporary, path)
}

#[cfg(windows)]
fn replace_file(temporary: &Path, path: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let temporary = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            temporary.as_ptr(),
            path.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    (result != 0)
        .then_some(())
        .ok_or_else(io::Error::last_os_error)
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            duration.as_millis().try_into().unwrap_or(u64::MAX)
        })
}

fn set_directory_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn set_file_permissions(file: &File, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        file.set_permissions(fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

fn platform_directory() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        return env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| home_directory().map(|home| home.join(".local/state")))
            .map(|root| root.join("terraleph"));
    }
    #[cfg(target_os = "macos")]
    {
        return home_directory().map(|home| home.join("Library/Application Support/Terraleph"));
    }
    #[cfg(windows)]
    {
        return env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| home_directory().map(|home| home.join("AppData/Local")))
            .map(|root| root.join("Terraleph"));
    }
    #[expect(
        unreachable_code,
        reason = "unsupported platforms have no history directory"
    )]
    None
}

#[cfg(target_os = "linux")]
fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

#[cfg(any(target_os = "macos", windows))]
fn home_directory() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

static TEMP_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tempfile::TempDir;

    use crate::app::{
        execution::{ExecutionContext, ExecutionTargetSpec, Tool},
        plan::PlanAction,
    };

    use super::*;

    fn key(address: &str, tool: Tool) -> HistoryKey {
        let context = ExecutionContext::loading("/project")
            .with_workspace("default")
            .with_tool(tool);
        let target = ExecutionTargetSpec {
            address: address.to_owned(),
            actions: vec![PlanAction::Update],
        };
        HistoryKey::for_target(&context, &target).expect("workspace is known")
    }

    fn success(key: HistoryKey, milliseconds: u64) -> SuccessfulTarget {
        SuccessfulTarget {
            key,
            duration: Duration::from_millis(milliseconds),
        }
    }

    fn load(store: &HistoryStore, key: &HistoryKey) -> Option<Duration> {
        store
            .load_many(&[Some(key.clone())])
            .into_iter()
            .next()
            .flatten()
    }

    #[test]
    fn stores_versioned_owner_only_json_and_reads_the_duration() {
        let fixture = TempDir::new().expect("test directory should be created");
        let root = fixture.path().join("history");
        let store = HistoryStore::new(root.clone());
        let key = key("terraform_data.api", Tool::Terraform);

        store
            .record(&[success(key.clone(), 1234)])
            .expect("history should be written");

        assert_eq!(load(&store, &key), Some(Duration::from_millis(1234)));
        assert_eq!(
            store.load_many(&[Some(key.clone()), None]),
            [Some(Duration::from_millis(1234)), None]
        );
        store
            .record(&[success(key.clone(), 5678)])
            .expect("newer history should replace the prior record");
        assert_eq!(load(&store, &key), Some(Duration::from_millis(5678)));
        let path = root.join(format!("{}.json", key.file_stem()));
        let document: Value = serde_json::from_slice(&fs::read(&path).expect("record exists"))
            .expect("record should be JSON");
        assert_eq!(document["version"], HISTORY_VERSION);
        assert_eq!(document["duration_ms"], 5678);
        assert!(document.get("recorded_at_unix_ms").is_some());
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&root)
                    .expect("root exists")
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path)
                    .expect("record exists")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn malformed_history_is_treated_as_missing() {
        let fixture = TempDir::new().expect("test directory should be created");
        let root = fixture.path().join("history");
        let store = HistoryStore::new(root.clone());
        let key = key("terraform_data.api", Tool::Terraform);
        fs::create_dir_all(&root).expect("root should exist");
        fs::write(root.join(format!("{}.json", key.file_stem())), b"{not-json")
            .expect("corrupt history should be written");

        assert_eq!(load(&store, &key), None);
    }

    #[test]
    fn concurrent_records_for_different_keys_survive_the_shared_lock() {
        let fixture = TempDir::new().expect("test directory should be created");
        let root = Arc::new(fixture.path().join("history"));
        let mut handles = Vec::new();
        for index in 0..8 {
            let root = Arc::clone(&root);
            handles.push(thread::spawn(move || {
                let store = HistoryStore::new((*root).clone());
                let key = key(&format!("terraform_data.target_{index}"), Tool::Terraform);
                store
                    .record(&[success(key, index)])
                    .expect("concurrent history should be written");
            }));
        }
        for handle in handles {
            handle.join().expect("history worker should finish");
        }

        let store = HistoryStore::new((*root).clone());
        for index in 0..8 {
            let key = key(&format!("terraform_data.target_{index}"), Tool::Terraform);
            assert_eq!(load(&store, &key), Some(Duration::from_millis(index)));
        }
    }

    #[test]
    fn write_failure_is_reported_without_creating_a_record() {
        let fixture = TempDir::new().expect("test directory should be created");
        let root = fixture.path().join("history");
        fs::write(&root, b"not a directory").expect("blocking file should be written");
        let store = HistoryStore::new(root);
        let key = key("terraform_data.api", Tool::Terraform);

        assert!(store.record(&[success(key, 1)]).is_err());
    }
}
