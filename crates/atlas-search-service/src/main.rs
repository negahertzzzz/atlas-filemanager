#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
pub mod arena_index;

#[cfg(not(windows))]
pub mod linux_daemon;

#[cfg(not(windows))]
fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let args: Vec<String> = std::env::args().collect();
    let foreground = args.iter().any(|a| a == "--console" || a == "-c");
    linux_daemon::run_daemon(foreground)
}

#[cfg(windows)]
mod mapped_index;

#[cfg(windows)]
mod windows_impl {
    use super::mapped_index;

    use anyhow::Context as _;
use atlas_core::search::service::{ServiceRequest, ServiceResponse, PIPE_PATH, PROTOCOL_VERSION};
use atlas_core::search::volume_index::{ChangeRecord, FileRecord, VolumeIndex};
use atlas_core::search::{
    SearchBackendStatus, SearchBatch, SearchCategory, SearchOrigin, SearchProgress, SearchQuery,
    SearchResult, SearchScope,
};
use atlas_win_space::ntfs::{self, CheckpointError};
use mapped_index::MappedVolume;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufWriter, Write as _};
use std::mem::size_of;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::null_mut;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use windows_service::service::{
    ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::{define_windows_service, service_control_handler, service_dispatcher};
use windows_sys::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_PIPE_CONNECTED, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    GetLogicalDrives, MoveFileExW, ReadFile, WriteFile, MOVEFILE_REPLACE_EXISTING,
    MOVEFILE_WRITE_THROUGH, PIPE_ACCESS_DUPLEX,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_MESSAGE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE, PIPE_WAIT,
};

const SERVICE_NAME: &str = "AtlasSearchService";

struct State {
    volumes: RwLock<Vec<(char, MappedVolume)>>,
    progress: RwLock<Vec<SearchProgress>>,
    latest_query: AtomicU64,
    search_gate: Mutex<()>,
    search_cache: Mutex<IncrementalSearchCache>,
}

#[derive(Default)]
struct IncrementalSearchCache {
    text: String,
    scope: SearchScope,
    filters: atlas_core::search::SearchFilters,
    matches: Vec<(char, u64)>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            volumes: RwLock::new(Vec::new()),
            progress: RwLock::new(Vec::new()),
            latest_query: AtomicU64::new(0),
            search_gate: Mutex::new(()),
            search_cache: Mutex::new(IncrementalSearchCache::default()),
        }
    }
}

pub fn main() -> anyhow::Result<()> {
    let arguments: Vec<String> = std::env::args().collect();
    if let Some(position) = arguments.iter().position(|argument| argument == "--probe") {
        let query = arguments
            .get(position + 1)
            .context("--probe requires a query")?;
        let state = State::default();
        let directory = data_directory();
        for letter in drive_letters() {
            let path = index_path(&directory, letter);
            if let Ok(volume) = MappedVolume::open(&path) {
                state
                    .volumes
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .push((letter, volume));
            }
        }
        let sequence = arguments.iter().any(|argument| argument == "--sequence");
        let queries: Vec<String> = if sequence {
            query
                .char_indices()
                .map(|(offset, character)| query[..offset + character.len_utf8()].to_owned())
                .collect()
        } else {
            vec![query.clone()]
        };
        for (index, text) in queries.iter().enumerate() {
            let generation = index as u64 + 1;
            let mut request = SearchQuery::text(text);
            request.generation = generation;
            request.limit = 25;
            request.interactive = sequence || arguments.iter().any(|argument| argument == "--interactive");
            let batch = execute_search(&state, &request);
            if index + 1 == queries.len() {
                for result in batch.results {
                    println!("{}", result.path.display());
                }
            }
        }
        if arguments.iter().any(|argument| argument == "--hold") {
            eprintln!("probe holding for 30 seconds (pid {})", std::process::id());
            thread::sleep(Duration::from_secs(30));
        }
        return Ok(());
    }
    if arguments.iter().any(|arg| arg == "--install") {
        return install_service();
    }
    if arguments.iter().any(|arg| arg == "--uninstall") {
        return uninstall_service();
    }
    if arguments.iter().any(|arg| arg == "--console") {
        return run(Arc::new(AtomicBool::new(false)));
    }
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)
        .context("start service dispatcher")?;
    Ok(())
}

fn install_service() -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    let current_exe = std::env::current_exe()?;
    let path_str = current_exe.to_string_lossy();
    let status = std::process::Command::new("sc.exe")
        .args([
            "create",
            SERVICE_NAME,
            &format!("binPath= \"{}\"", path_str),
            "start=",
            "auto",
            "DisplayName=",
            "Atlas Search Service",
        ])
        .creation_flags(0x0800_0000)
        .status()?;
    if !status.success() {
        anyhow::bail!("sc.exe create failed with status {status}");
    }
    eprintln!("Service '{SERVICE_NAME}' registered with start=auto.");
    let start_status = std::process::Command::new("sc.exe")
        .args(["start", SERVICE_NAME])
        .creation_flags(0x0800_0000)
        .status()?;
    if start_status.success() {
        eprintln!("Service '{SERVICE_NAME}' started successfully.");
    } else {
        eprintln!("Service '{SERVICE_NAME}' registered; start returned {start_status}");
    }
    Ok(())
}

fn uninstall_service() -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;
    let _ = std::process::Command::new("sc.exe")
        .args(["stop", SERVICE_NAME])
        .creation_flags(0x0800_0000)
        .status();
    let status = std::process::Command::new("sc.exe")
        .args(["delete", SERVICE_NAME])
        .creation_flags(0x0800_0000)
        .status()?;
    if !status.success() {
        anyhow::bail!("sc.exe delete failed with status {status}");
    }
    eprintln!("Service '{SERVICE_NAME}' uninstalled successfully.");
    Ok(())
}

define_windows_service!(ffi_service_main, service_main);

fn service_main(_arguments: Vec<OsString>) {
    let stop = Arc::new(AtomicBool::new(false));
    let handler_stop = stop.clone();
    let Ok(handle) = service_control_handler::register(
        SERVICE_NAME,
        move |control| {
            match control {
        ServiceControl::Stop | ServiceControl::Shutdown => { handler_stop.store(true, Ordering::Release); windows_service::service_control_handler::ServiceControlHandlerResult::NoError }
        ServiceControl::Interrogate => windows_service::service_control_handler::ServiceControlHandlerResult::NoError,
        _ => windows_service::service_control_handler::ServiceControlHandlerResult::NotImplemented,
    }
        },
    ) else {
        return;
    };
    let _ = handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Running,
        controls_accepted: ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN,
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    });
    let _ = run(stop);
    let _ = handle.set_service_status(ServiceStatus {
        service_type: ServiceType::OWN_PROCESS,
        current_state: ServiceState::Stopped,
        controls_accepted: ServiceControlAccept::empty(),
        exit_code: ServiceExitCode::Win32(0),
        checkpoint: 0,
        wait_hint: Duration::ZERO,
        process_id: None,
    });
}

fn run(stop: Arc<AtomicBool>) -> anyhow::Result<()> {
    use windows_sys::Win32::Foundation::ERROR_ALREADY_EXISTS;
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let mutex_name: Vec<u16> = OsStr::new(r"Global\AtlasSearchServiceSingleInstance")
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mutex = unsafe { CreateMutexW(null_mut(), 1, mutex_name.as_ptr()) };
    if unsafe { windows_sys::Win32::Foundation::GetLastError() } == ERROR_ALREADY_EXISTS {
        eprintln!("Atlas Search Service is already running.");
        return Ok(());
    }

    eprintln!("Atlas Search Service {}", env!("CARGO_PKG_VERSION"));
    eprintln!("Index directory: {}", data_directory().display());
    let state = Arc::new(State::default());
    let worker_state = state.clone();
    let worker_stop = stop.clone();
    thread::spawn(move || index_loop(&worker_state, &worker_stop));
    let result = serve_pipe(state, &stop);
    if !mutex.is_null() && mutex != windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
        unsafe { CloseHandle(mutex) };
    }
    result
}

fn index_loop(state: &State, stop: &AtomicBool) {
    let data_dir = data_directory();
    let _ = fs::create_dir_all(&data_dir);
    for letter in drive_letters() {
        if stop.load(Ordering::Acquire) {
            return;
        }
        set_progress(state, letter, SearchBackendStatus::Loading, 0);
        match load_or_build(letter, &data_dir) {
            Ok(index) => {
                let count = index.len() as u64;
                state
                    .volumes
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((letter, index));
                set_progress(state, letter, SearchBackendStatus::Ready, count);
            }
            Err(error) => set_progress(
                state,
                letter,
                SearchBackendStatus::Error(error.to_string()),
                0,
            ),
        }
    }
    while !stop.load(Ordering::Acquire) {
        {
            let mut volumes = state.volumes.write().unwrap_or_else(|e| e.into_inner());
            for (letter, index) in volumes.iter_mut() {
                match replay_mapped(*letter, index) {
                    Err(error) => match error {
                        CheckpointError::JournalChanged | CheckpointError::JournalTruncated => {
                            set_progress(state, *letter, SearchBackendStatus::Loading, 0);
                            let path = index_path(&data_dir, *letter);
                            match rebuild(*letter, &path).and_then(|rebuilt| {
                                drop(rebuilt);
                                MappedVolume::open(&path)
                            }) {
                                Ok(rebuilt) => {
                                    *index = rebuilt;
                                    set_progress(
                                        state,
                                        *letter,
                                        SearchBackendStatus::Ready,
                                        index.len() as u64,
                                    );
                                }
                                Err(rebuild_error) => set_progress(
                                    state,
                                    *letter,
                                    SearchBackendStatus::Error(rebuild_error.to_string()),
                                    0,
                                ),
                            }
                        }
                        CheckpointError::Io(_) => {
                            if index.len() == 0 {
                                set_progress(
                                    state,
                                    *letter,
                                    SearchBackendStatus::Error(error.to_string()),
                                    0,
                                );
                            }
                        }
                    },
                    Ok(()) => {}
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    }
}

fn load_or_build(letter: char, data_dir: &Path) -> anyhow::Result<MappedVolume> {
    let path = index_path(data_dir, letter);
    if path.exists() {
        let load_started = Instant::now();
        if let Ok(mut index) = MappedVolume::open(&path) {
            let count = index.len();
            eprintln!(
                "[{letter}:\\] loaded {count} persisted records in {:.2}s",
                load_started.elapsed().as_secs_f64()
            );
            let replay_started = Instant::now();
            match replay_mapped(letter, &mut index) {
                Ok(()) => {
                    eprintln!(
                        "[{letter}:\\] replayed pending USN changes in {:.3}s",
                        replay_started.elapsed().as_secs_f64()
                    );
                    return Ok(index);
                }
                Err(CheckpointError::Io(err)) => {
                    eprintln!(
                        "[{letter}:\\] continuing with persisted records (USN replay unavailable: {err})"
                    );
                    return Ok(index);
                }
                Err(err) => {
                    eprintln!("[{letter}:\\] journal error ({err:?}), rebuilding");
                }
            }
        }
    }
    // One-time migration from the schema-v1 JSON produced by development builds.
    let legacy = data_dir.join(format!("index-{}.json", letter.to_ascii_uppercase()));
    if let Ok(bytes) = fs::read(&legacy) {
        if let Ok(mut index) = serde_json::from_slice::<VolumeIndex>(&bytes) {
            eprintln!("[{letter}:\\] migrating legacy JSON snapshot to compact binary");
            if replay(letter, &mut index).is_ok() {
                save_index(&index, &path)?;
                let _ = fs::remove_file(&legacy);
                drop(index);
                return MappedVolume::open(&path);
            }
        }
    }
    let index = rebuild(letter, &path)?;
    drop(index);
    MappedVolume::open(&path)
}

fn replay_mapped(letter: char, index: &mut MappedVolume) -> Result<(), CheckpointError> {
    let state = ntfs::read_journal(letter, index.journal_id, index.next_usn, |record| {
        index.apply(record);
        true
    })?;
    index.next_usn = state.next_usn;
    Ok(())
}

fn rebuild(letter: char, path: &Path) -> anyhow::Result<VolumeIndex> {
    let total_started = Instant::now();
    let before = ntfs::query_journal(letter)?;
    let mut records = Vec::new();
    let mft_started = Instant::now();
    ntfs::enumerate_mft(letter, |record| {
        records.push(FileRecord {
            id: record.file_reference,
            parent: record.parent_reference,
            name: record.name,
            is_dir: record.is_dir,
            size: 0,
            modified: None,
        });
        let count = records.len();
        if count % 100_000 == 0 {
            let elapsed = mft_started.elapsed().as_secs_f64();
            eprintln!(
                "[{letter}:\\] MFT {count:>9} records | {:>10.0} records/s | {:>6.2}s",
                count as f64 / elapsed.max(f64::EPSILON),
                elapsed
            );
        }
        true
    })?;
    let mft_elapsed = mft_started.elapsed();
    eprintln!(
        "[{letter}:\\] MFT complete: {} records in {:.2}s ({:.0} records/s)",
        records.len(),
        mft_elapsed.as_secs_f64(),
        records.len() as f64 / mft_elapsed.as_secs_f64().max(f64::EPSILON)
    );
    let root_id = records
        .iter()
        .find(|record| record.id & 0x0000_FFFF_FFFF_FFFF == 5)
        .map(|record| record.id)
        .unwrap_or(5);
    let mut index = VolumeIndex::new(format!("{letter}:"), before.id, before.next_usn, root_id);
    index.replace_records(records);
    let replay_started = Instant::now();
    replay(letter, &mut index)
        .map_err(|error| anyhow::anyhow!("journal changed during rebuild: {error:?}"))?;
    eprintln!(
        "[{letter}:\\] USN catch-up complete in {:.3}s",
        replay_started.elapsed().as_secs_f64()
    );
    let save_started = Instant::now();
    save_index(&index, path)?;
    eprintln!(
        "[{letter}:\\] snapshot written in {:.2}s; total initial index {:.2}s",
        save_started.elapsed().as_secs_f64(),
        total_started.elapsed().as_secs_f64()
    );
    Ok(index)
}

fn replay(letter: char, index: &mut VolumeIndex) -> Result<Vec<(u64, String)>, CheckpointError> {
    let mut additions = Vec::new();
    let state = ntfs::read_journal(letter, index.journal_id, index.next_usn, |record| {
        let change = ChangeRecord {
            id: record.file_reference,
            parent: record.parent_reference,
            name: record.name,
            is_dir: record.is_dir,
            reason: record.reason,
        };
        if change.reason
            & (atlas_core::search::volume_index::REASON_FILE_CREATE
                | atlas_core::search::volume_index::REASON_RENAME_NEW_NAME)
            != 0
        {
            additions.push((change.id, change.name.clone()));
        }
        index.apply(change);
        true
    })?;
    index.next_usn = state.next_usn;
    Ok(additions)
}

fn save_index(index: &VolumeIndex, path: &Path) -> anyhow::Result<()> {
    let temporary = path.with_extension("tmp");
    let file = fs::File::create(&temporary)?;
    let mut writer = BufWriter::with_capacity(4 * 1024 * 1024, file);
    index.write_binary(&mut writer)?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    let from: Vec<u16> = temporary.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    if unsafe {
        MoveFileExW(
            from.as_ptr(),
            to.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn serve_pipe(state: Arc<State>, stop: &AtomicBool) -> anyhow::Result<()> {
    let wide: Vec<u16> = OsStr::new(PIPE_PATH).encode_wide().chain(Some(0)).collect();
    let sddl: Vec<u16> = OsStr::new("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;AU)")
        .encode_wide()
        .chain(Some(0))
        .collect();
    while !stop.load(Ordering::Acquire) {
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.cast(),
            bInheritHandle: 0,
        };
        let pipe = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                16,
                64 * 1024,
                64 * 1024,
                1000,
                &security,
            )
        };
        unsafe {
            LocalFree(descriptor.cast());
        }
        if pipe == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error().into());
        }
        let connected = unsafe { ConnectNamedPipe(pipe, null_mut()) } != 0
            || io::Error::last_os_error().raw_os_error() == Some(ERROR_PIPE_CONNECTED as i32);
        if connected {
            let client_state = state.clone();
            let pipe_value = pipe as isize;
            thread::spawn(move || {
                let pipe = pipe_value as HANDLE;
                handle_client(pipe, &client_state);
                unsafe {
                    DisconnectNamedPipe(pipe);
                    CloseHandle(pipe);
                }
            });
        } else {
            unsafe {
                DisconnectNamedPipe(pipe);
                CloseHandle(pipe);
            }
        }
    }
    Ok(())
}

fn handle_client(pipe: HANDLE, state: &State) {
    let mut input = vec![0_u8; 64 * 1024];
    let mut read = 0;
    if unsafe {
        ReadFile(
            pipe,
            input.as_mut_ptr(),
            input.len() as u32,
            &mut read,
            null_mut(),
        )
    } == 0
    {
        return;
    }
    let response = match serde_json::from_slice::<ServiceRequest>(&input[..read as usize]) {
        Ok(ServiceRequest::Hello { protocol }) if protocol == PROTOCOL_VERSION => {
            ServiceResponse::Hello {
                protocol: PROTOCOL_VERSION,
                schema: 1,
            }
        }
        Ok(ServiceRequest::Hello { .. }) => ServiceResponse::Error("incompatible protocol".into()),
        Ok(ServiceRequest::Status) => ServiceResponse::Status(
            state
                .progress
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        ),
        Ok(ServiceRequest::Search(query)) => {
            let mut batch = execute_search(state, &query);
            match visible_to_client(pipe, std::mem::take(&mut batch.results)) {
                Ok(results) => {
                    batch.results = results;
                    ServiceResponse::Search(batch)
                }
                Err(error) => ServiceResponse::Error(error),
            }
        }
        Err(error) => ServiceResponse::Error(error.to_string()),
    };
    let Ok(bytes) = serde_json::to_vec(&response) else {
        return;
    };
    let mut written = 0;
    unsafe {
        WriteFile(
            pipe,
            bytes.as_ptr(),
            bytes.len() as u32,
            &mut written,
            null_mut(),
        );
    }
}

/// The index is built from every volume's MFT by a LocalSystem service, so it
/// names files inside other users' private folders. Hand each client only what
/// it could list itself: impersonate the pipe client and keep the results its
/// own token can read the attributes of. Fails closed when the client cannot
/// be impersonated.
fn visible_to_client(pipe: HANDLE, results: Vec<SearchResult>) -> Result<Vec<SearchResult>, String> {
    use windows_sys::Win32::Security::RevertToSelf;
    use windows_sys::Win32::System::Pipes::ImpersonateNamedPipeClient;

    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            // Never keep serving with a client's identity on this thread.
            if unsafe { RevertToSelf() } == 0 {
                std::process::abort();
            }
        }
    }

    if results.is_empty() {
        return Ok(results);
    }
    if unsafe { ImpersonateNamedPipeClient(pipe) } == 0 {
        return Err(format!(
            "cannot verify the client's access: {}",
            io::Error::last_os_error()
        ));
    }
    let _revert = Revert;
    Ok(results
        .into_iter()
        .filter(|result| fs::symlink_metadata(&result.path).is_ok())
        .collect())
}

fn execute_search(state: &State, query: &SearchQuery) -> SearchBatch {
    // Client generations identify replies within one app session. They can
    // restart at zero or overlap another client's counter, so cancellation
    // must use the service's own request order.
    let request_id = state.latest_query.fetch_add(1, Ordering::AcqRel) + 1;
    let _gate = state
        .search_gate
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if state.latest_query.load(Ordering::Acquire) != request_id {
        return SearchBatch {
            generation: query.generation,
            results: Vec::new(),
            complete: false,
        };
    }
    search_index(state, query, request_id)
}

fn search_index(state: &State, query: &SearchQuery, request_id: u64) -> SearchBatch {
    let started = Instant::now();
    let volumes = state.volumes.read().unwrap_or_else(|e| e.into_inner());
    let candidate_limit = query
        .offset
        .saturating_add(query.limit.max(1))
        .saturating_mul(4)
        .max(256);
    let mut candidates = BinaryHeap::<Reverse<(i64, char, u64)>>::new();
    let mut canceled = false;
    let mut name_examined = 0_usize;
    let mut path_examined = 0_usize;
    let name_budget = if query.interactive {
        match query.text.chars().count() {
            0 | 1 => 25_000,
            2 => 75_000,
            3 => 125_000,
            _ => usize::MAX,
        }
    } else {
        usize::MAX
    };
    let mut name_budget_exhausted = false;
    let required_mask = mapped_index::ascii_mask(query.text.as_bytes());
    let compact_query: String = query
        .text
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect();
    let include_path = compact_query.chars().count() >= 4;
    let cached_matches = {
        let cache = state
            .search_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        (query.text.len() > cache.text.len()
            && query.text.starts_with(&cache.text)
            && query.scope == cache.scope
            && query.filters == cache.filters
            && !cache.matches.is_empty())
        .then(|| cache.matches.clone())
    };
    const MAX_INCREMENTAL_MATCHES: usize = 250_000;
    let mut incremental_matches = Vec::new();
    let mut incremental_overflow = false;
    let mut navigation_noise = std::collections::HashMap::new();
    {
        let mut consider = |letter: char, record: mapped_index::RecordRef<'_>| {
            if name_examined >= name_budget {
                name_budget_exhausted = true;
                return false;
            }
            name_examined += 1;
            if state.latest_query.load(Ordering::Acquire) != request_id {
                canceled = true;
                return false;
            }
            if !query.filters.categories.is_empty() {
                let category = if record.is_dir {
                    SearchCategory::Folder
                } else {
                    SearchCategory::File
                };
                if !query.filters.categories.contains(&category) {
                    return true;
                }
            }
            if query.filters.min_size.is_some_and(|min| record.size < min)
                || query.filters.max_size.is_some_and(|max| record.size > max)
            {
                return true;
            }
            let Some(score) = score_name(&record.name, &query.text) else {
                return true;
            };
            if !incremental_overflow {
                if incremental_matches.len() < MAX_INCREMENTAL_MATCHES {
                    incremental_matches.push((letter, record.id));
                } else {
                    incremental_matches.clear();
                    incremental_overflow = true;
                }
            }
            let score = if query.interactive {
                let noise = volumes.iter().find(|(candidate, _)| *candidate == letter)
                    .map_or(0, |(_, volume)| record_navigation_noise(volume, letter, record.id, &query.text, &mut navigation_noise));
                score - i64::from(noise) * 5_000
            } else {
                score
            };
            insert_candidate(&mut candidates, candidate_limit, score, letter, record.id);
            true
        };

        if let Some(cached_matches) = cached_matches {
            for (letter, id) in cached_matches {
                let Some((_, volume)) = volumes.iter().find(|(candidate, _)| *candidate == letter)
                else {
                    continue;
                };
                let Some(record) = volume.record(id) else {
                    continue;
                };
                if !consider(letter, record) {
                    break;
                }
            }
        } else {
            for (letter, volume) in volumes.iter() {
                let mut visit = |record: mapped_index::RecordRef<'_>| consider(*letter, record);
                volume.for_each_name_candidate(required_mask, &mut visit);
                if state.latest_query.load(Ordering::Acquire) != request_id {
                    break;
                }
            }
        }
    }
    if !canceled
        && !name_budget_exhausted
        && !incremental_overflow
        && query.text.chars().count() >= 2
    {
        let mut cache = state
            .search_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        cache.text.clone_from(&query.text);
        cache.scope.clone_from(&query.scope);
        cache.filters.clone_from(&query.filters);
        cache.matches = incremental_matches;
    }
    let name_elapsed = started.elapsed();
    // Name matches are preferred. Only pay for a full-path recovery pass when
    // they cannot fill the requested page; this keeps ordinary typing fast
    // while still supporting concatenated component queries such as
    // "fzemiskyblock".
    if include_path && candidates.len() < query.offset.saturating_add(query.limit.max(1)) {
        let characters: Vec<char> = compact_query.chars().collect();
        let mut suffix_lengths: Vec<_> = [8_usize, 6, 4, 3]
            .into_iter()
            .map(|length| length.min(characters.len()))
            .collect();
        suffix_lengths.sort_unstable_by(|left, right| right.cmp(left));
        suffix_lengths.dedup();
        let mut checked = HashSet::new();
        for suffix_length in suffix_lengths {
            let suffix: String = characters[characters.len() - suffix_length..]
                .iter()
                .collect();
            let suffix_mask = mapped_index::ascii_mask(suffix.as_bytes());
            for (letter, volume) in volumes.iter() {
                let root = PathBuf::from(format!("{letter}:\\"));
                let mut visit = |record: mapped_index::RecordRef<'_>| {
                    path_examined += 1;
                    if state.latest_query.load(Ordering::Acquire) != request_id {
                        canceled = true;
                        return false;
                    }
                    if !checked.insert((*letter, record.id))
                        || score_name(&record.name, &suffix).is_none()
                    {
                        return true;
                    }
                    if !query.filters.categories.is_empty() {
                        let category = if record.is_dir {
                            SearchCategory::Folder
                        } else {
                            SearchCategory::File
                        };
                        if !query.filters.categories.contains(&category) {
                            return true;
                        }
                    }
                    if query.filters.min_size.is_some_and(|min| record.size < min)
                        || query.filters.max_size.is_some_and(|max| record.size > max)
                    {
                        return true;
                    }
                    let Some(path) = volume.path_for(record.id, &root) else {
                        return true;
                    };
                    let Some(score) = score_path(&path, &compact_query) else {
                        return true;
                    };
                    let score = if query.interactive {
                        score - i64::from(atlas_core::search::navigation::path_noise(&path, &query.text)) * 5_000
                    } else { score };
                    insert_candidate(&mut candidates, candidate_limit, score, *letter, record.id);
                    true
                };
                volume.for_each_name_candidate(suffix_mask, &mut visit);
                if canceled {
                    break;
                }
            }
            if canceled || candidates.len() >= query.offset.saturating_add(query.limit.max(1)) {
                break;
            }
        }
    }
    let path_elapsed = started.elapsed().saturating_sub(name_elapsed);
    let rank_started = Instant::now();
    let mut ranked: Vec<_> = candidates.into_iter().map(|value| value.0).collect();
    let candidate_count = ranked.len();
    ranked.sort_unstable_by(|left, right| right.0.cmp(&left.0));
    let rank_elapsed = rank_started.elapsed();
    let materialize_started = Instant::now();
    let mut results = Vec::with_capacity(query.limit.max(1));
    let mut reported_path_failure = false;
    for (score, letter, id) in ranked {
        let Some((_, volume)) = volumes.iter().find(|(candidate, _)| *candidate == letter) else {
            continue;
        };
        let Some(record) = volume.record(id) else {
            continue;
        };
        let root = PathBuf::from(format!("{letter}:\\"));
        let Some(path) = volume.path_for(id, &root) else {
            if !reported_path_failure {
                eprintln!(
                    "[search] unresolved path: volume={letter}: id={} parent={} root={} name={:?}",
                    record.id, record.parent, volume.root_id, record.name
                );
                reported_path_failure = true;
            }
            continue;
        };
        if !matches!(&query.scope, SearchScope::Everything) {
            let scope = match &query.scope {
                SearchScope::Volume(path)
                | SearchScope::Folder(path)
                | SearchScope::Location(path) => path,
                SearchScope::Everything => unreachable!(),
            };
            if !path.starts_with(scope) {
                continue;
            }
        }
        if !record.is_dir && !query.filters.extensions.is_empty() {
            let extension = path
                .extension()
                .map(|value| value.to_string_lossy().to_lowercase());
            if !query.filters.extensions.iter().any(|value| {
                extension.as_deref() == Some(value.trim_start_matches('.').to_lowercase().as_str())
            }) {
                continue;
            }
        }
        if atlas_core::vault::is_private_path(&path) { continue; }
        results.push(SearchResult {
            path,
            is_dir: record.is_dir,
            size: record.size,
            modified: record.modified,
            origin: SearchOrigin::WindowsService,
            score,
        });
        if results.len() >= query.offset.saturating_add(query.limit.max(1)) {
            break;
        }
    }
    results.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.path.cmp(&right.path))
    });
    let results: Vec<SearchResult> = results
        .into_iter()
        .skip(query.offset)
        .take(query.limit.max(1))
        .collect();
    eprintln!(
        "[search] {:?}: examined {name_examined} names + {path_examined} path candidates; \
         kept {candidate_count}, {} results in {:.1}ms \
         [name {:.1} | path {:.1} | rank {:.1} | materialize {:.1}]",
        query.text,
        results.len(),
        started.elapsed().as_secs_f64() * 1_000.0,
        name_elapsed.as_secs_f64() * 1_000.0,
        path_elapsed.as_secs_f64() * 1_000.0,
        rank_elapsed.as_secs_f64() * 1_000.0,
        materialize_started.elapsed().as_secs_f64() * 1_000.0,
    );
    SearchBatch {
        generation: query.generation,
        results,
        complete: !canceled,
    }
}

fn insert_candidate(
    candidates: &mut BinaryHeap<Reverse<(i64, char, u64)>>,
    limit: usize,
    score: i64,
    letter: char,
    id: u64,
) {
    let candidate = Reverse((score, letter, id));
    if candidates.len() < limit {
        candidates.push(candidate);
    } else if candidates
        .peek()
        .is_some_and(|worst| candidate.0 .0 > worst.0 .0)
    {
        candidates.pop();
        candidates.push(candidate);
    }
}

/// Classify ancestors from the mapped catalog, with no filesystem I/O. Cache
/// shared parents so histories containing thousands of matches stay cheap.
fn record_navigation_noise(
    volume: &MappedVolume,
    letter: char,
    id: u64,
    query: &str,
    cache: &mut std::collections::HashMap<(char, u64), u8>,
) -> u8 {
    let mut current = id;
    let mut chain = Vec::new();
    let mut inherited = 2;
    const RECORD_NUMBER_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;
    for _ in 0..128 {
        if current & RECORD_NUMBER_MASK == volume.root_id & RECORD_NUMBER_MASK {
            inherited = 0;
            break;
        }
        if let Some(&noise) = cache.get(&(letter, current)) {
            inherited = noise;
            break;
        }
        let Some(record) = volume.record(current) else { break; };
        chain.push((current, atlas_core::search::navigation::component_noise(record.name, query)));
        current = record.parent;
    }
    for (id, noise) in chain.into_iter().rev() {
        inherited = inherited.max(noise);
        cache.insert((letter, id), inherited);
    }
    inherited
}

fn score_path(path: &Path, query: &str) -> Option<i64> {
    let compact_path: String = path
        .to_string_lossy()
        .chars()
        .filter(|character| character.is_alphanumeric())
        .collect();
    score_name(&compact_path, query).map(|score| score - 600)
}

fn score_name(name: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let sensitive = query.bytes().any(|byte| byte.is_ascii_uppercase());
    if name.is_ascii() && query.is_ascii() {
        return score_ascii_name(name.as_bytes(), query.as_bytes(), sensitive);
    }
    let candidate = atlas_core::search::navigation::normalized_name(name, sensitive);
    let needle = atlas_core::search::navigation::normalized_name(query, sensitive);
    if candidate == needle {
        return Some(4_000);
    }
    if candidate.starts_with(&needle) {
        return Some(3_000 - candidate.len() as i64);
    }
    if candidate.contains(&needle) {
        return Some(2_000 - candidate.len() as i64);
    }
    if candidate.len().abs_diff(needle.len()) <= 2 {
        if let Some(distance) =
            damerau_distance_at_most_two(candidate.as_bytes(), needle.as_bytes())
        {
            return Some(1_500 - i64::from(distance) * 100 - candidate.len() as i64);
        }
    }
    let mut chars = candidate.chars();
    needle
        .chars()
        .all(|wanted| chars.any(|found| found == wanted))
        .then_some(1_000 - candidate.len() as i64)
}

fn score_ascii_name(name: &[u8], query: &[u8], sensitive: bool) -> Option<i64> {
    let equal = |left: u8, right: u8| {
        let is_separator = |byte: u8| byte.is_ascii_whitespace() || matches!(byte, b'-' | b'_');
        if is_separator(left) && is_separator(right) {
            return true;
        }
        if sensitive {
            left == right
        } else {
            left.eq_ignore_ascii_case(&right)
        }
    };
    if name.len() == query.len() && name.iter().zip(query).all(|(&a, &b)| equal(a, b)) {
        return Some(4_000);
    }
    if name.len() >= query.len()
        && name[..query.len()]
            .iter()
            .zip(query)
            .all(|(&a, &b)| equal(a, b))
    {
        return Some(3_000 - name.len() as i64);
    }
    if !query.is_empty()
        && name
            .windows(query.len())
            .any(|window| window.iter().zip(query).all(|(&a, &b)| equal(a, b)))
    {
        return Some(2_000 - name.len() as i64);
    }
    if name.len().abs_diff(query.len()) <= 2 {
        let mut folded_name = [0_u8; 64];
        let mut folded_query = [0_u8; 64];
        if name.len() <= 63 && query.len() <= 63 {
            for (target, &byte) in folded_name.iter_mut().zip(name) {
                *target = if sensitive {
                    byte
                } else {
                    byte.to_ascii_lowercase()
                };
            }
            for (target, &byte) in folded_query.iter_mut().zip(query) {
                *target = if sensitive {
                    byte
                } else {
                    byte.to_ascii_lowercase()
                };
            }
            if let Some(distance) = damerau_distance_at_most_two(
                &folded_name[..name.len()],
                &folded_query[..query.len()],
            ) {
                return Some(1_500 - i64::from(distance) * 100 - name.len() as i64);
            }
        }
    }
    let mut position = 0;
    for &wanted in query {
        let offset = name[position..]
            .iter()
            .position(|&found| equal(found, wanted))?;
        position += offset + 1;
    }
    Some(1_000 - name.len() as i64)
}

fn damerau_distance_at_most_two(left: &[u8], right: &[u8]) -> Option<u8> {
    if left.len() > 63 || right.len() > 63 {
        return None;
    }
    let mut previous_previous = [0_usize; 64];
    let mut previous = [0_usize; 64];
    let mut current = [0_usize; 64];
    for (index, value) in previous.iter_mut().take(right.len() + 1).enumerate() {
        *value = index;
    }
    for i in 1..=left.len() {
        current[0] = i;
        let mut row_min = i;
        for j in 1..=right.len() {
            let cost = usize::from(left[i - 1] != right[j - 1]);
            current[j] = (previous[j] + 1)
                .min(current[j - 1] + 1)
                .min(previous[j - 1] + cost);
            if i > 1 && j > 1 && left[i - 1] == right[j - 2] && left[i - 2] == right[j - 1] {
                current[j] = current[j].min(previous_previous[j - 2] + 1);
            }
            row_min = row_min.min(current[j]);
        }
        if row_min > 2 {
            return None;
        }
        previous_previous = previous;
        previous = current;
    }
    (previous[right.len()] <= 2).then_some(previous[right.len()] as u8)
}

fn set_progress(state: &State, letter: char, status: SearchBackendStatus, indexed: u64) {
    let source = format!("{letter}:\\");
    match &status {
        SearchBackendStatus::Loading => eprintln!("[{source}] reading NTFS MFT..."),
        SearchBackendStatus::Ready => eprintln!("[{source}] ready: {indexed} records"),
        SearchBackendStatus::Disconnected => eprintln!("[{source}] disconnected"),
        SearchBackendStatus::Fallback => eprintln!("[{source}] fallback"),
        SearchBackendStatus::Error(error) => eprintln!("[{source}] error: {error}"),
    }
    let mut progress = state.progress.write().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = progress.iter_mut().find(|item| item.source == source) {
        existing.status = status;
        existing.indexed = indexed;
    } else {
        progress.push(SearchProgress {
            source,
            status,
            indexed,
            total: None,
        });
    }
}

fn data_directory() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("Atlas")
        .join("Search")
}
fn index_path(directory: &Path, letter: char) -> PathBuf {
    directory.join(format!("index-{}.atlasidx", letter.to_ascii_uppercase()))
}
fn is_ntfs_drive(letter: char) -> bool {
    use windows_sys::Win32::Storage::FileSystem::{GetDriveTypeW, GetVolumeInformationW};
    let root = format!(r"{letter}:\\");
    let wide: Vec<u16> = OsStr::new(&root).encode_wide().chain(Some(0)).collect();
    let drive_type = unsafe { GetDriveTypeW(wide.as_ptr()) };
    // 2 = DRIVE_REMOVABLE, 3 = DRIVE_FIXED
    if drive_type != 2 && drive_type != 3 {
        return false;
    }
    let mut fs_name = [0u16; 32];
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            fs_name.as_mut_ptr(),
            fs_name.len() as u32,
        )
    };
    if ok == 0 {
        return false;
    }
    let len = fs_name.iter().position(|&c| c == 0).unwrap_or(fs_name.len());
    let name = String::from_utf16_lossy(&fs_name[..len]);
    name.eq_ignore_ascii_case("NTFS")
}

fn drive_letters() -> Vec<char> {
    let mask = unsafe { GetLogicalDrives() };
    let data_dir = data_directory();
    (0..26)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| (b'A' + bit) as char)
        .filter(|&letter| is_ntfs_drive(letter) || index_path(&data_dir, letter).exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restarted_client_generations_still_return_folder_results() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "atlas-search-generation-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("index-C.atlasidx");
        let mut index = VolumeIndex::new("C:".into(), 1, 0, 5);
        index.replace_records([
            FileRecord {
                id: 5,
                parent: 5,
                name: String::new(),
                is_dir: true,
                size: 0,
                modified: None,
            },
            FileRecord {
                id: 6,
                parent: (1 << 48) | 5,
                name: "egui-app".into(),
                is_dir: true,
                size: 0,
                modified: None,
            },
        ]);
        index.apply(atlas_core::search::volume_index::ChangeRecord {
            id: 7, parent: (1 << 48) | 5, name: ".claude".into(), is_dir: true,
            reason: atlas_core::search::volume_index::REASON_FILE_CREATE,
        });
        // More history copies than the candidate heap can keep. Ranking must
        // prefer the real project before the page is truncated.
        for id in 8..308 {
            index.apply(atlas_core::search::volume_index::ChangeRecord {
                id, parent: 7, name: format!("C--Users-finder-project-OS-Vida-{id}"), is_dir: true,
                reason: atlas_core::search::volume_index::REASON_FILE_CREATE,
            });
        }
        index.apply(atlas_core::search::volume_index::ChangeRecord {
            id: 308, parent: (1 << 48) | 5, name: "OS Vida".into(), is_dir: true,
            reason: atlas_core::search::volume_index::REASON_FILE_CREATE,
        });
        save_index(&index, &path).unwrap();
        let state = State::default();
        state.volumes.write().unwrap().push(('C', MappedVolume::open(&path).unwrap()));
        for generation in [1_000, 1, 0, 1] {
            let mut query = SearchQuery::text("egui");
            query.generation = generation;
            query.limit = 1;
            let batch = execute_search(&state, &query);
            assert_eq!(batch.generation, generation);
            assert!(batch.complete);
            assert_eq!(batch.results.len(), 1);
            assert_eq!(batch.results[0].path, PathBuf::from(r"C:\egui-app"));
            assert!(batch.results[0].is_dir);
        }
        let mut query = SearchQuery::text("os-vida");
        query.interactive = true;
        query.limit = 1;
        let batch = execute_search(&state, &query);
        assert!(batch.complete);
        assert_eq!(batch.results[0].path, PathBuf::from(r"C:\OS Vida"));
        drop(state);
        fs::remove_file(path.with_extension("atlasfind2")).unwrap();
        fs::remove_file(path).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn fuzzy_name_accepts_lazy_subsequence() {
        assert!(score_name("SkyBlockAnsestral", "skbans").is_some());
        assert!(score_name("SkyBlockAnsestral", "sba").is_some());
        assert!(score_name("SkyBlockAnsestral", "snab").is_none());
    }

    #[test]
    fn compact_path_matches_across_components() {
        assert!(score_path(
            Path::new(r"C:\Users\fzemi\Downloads\SkyBlockAnsestral"),
            "fzemiskyblock"
        )
        .is_some());
    }
}
}

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    windows_impl::main()
}
