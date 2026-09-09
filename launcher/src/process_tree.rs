//! Services enter a kill-on-close job atomically at process creation. FFmpeg and
//! any later descendants inherit it, even if their immediate parent has died.
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::File,
    mem::size_of,
    os::windows::{
        ffi::OsStrExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::Path,
    process::ExitStatus,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, bail, ensure};
use windows::{
    Win32::{
        Foundation::{
            DUPLICATE_SAME_ACCESS, DuplicateHandle, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA,
            HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        System::{
            JobObjects::{
                CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_BASIC_PROCESS_ID_LIST,
                JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectBasicAccountingInformation,
                JobObjectBasicProcessIdList, JobObjectExtendedLimitInformation,
                QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
            },
            Threading::{
                CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT,
                CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
                GetCurrentProcess, GetExitCodeProcess, InitializeProcThreadAttributeList,
                LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
                PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION, PROCESS_NAME_FORMAT,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
                QueryFullProcessImageNameW, STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess,
                UpdateProcThreadAttribute, WaitForSingleObject,
            },
        },
    },
    core::{PCWSTR, PWSTR},
};

pub struct ServiceJob(OwnedHandle);

impl ServiceJob {
    pub fn new() -> anyhow::Result<Self> {
        let job = Self(unsafe { OwnedHandle::from_raw_handle(CreateJobObjectW(None, None)?.0) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                raw(&job.0),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }
        Ok(job)
    }

    pub fn spawn(
        &self,
        executable: &Path,
        arguments: &[String],
        directory: &Path,
        log: &File,
        environment: &BTreeMap<&str, String>,
    ) -> anyhow::Result<ManagedChild> {
        let application = wide(executable.as_os_str())?;
        let directory = wide(directory.as_os_str())?;
        let mut command_line = quote_argument(executable.as_os_str())?;
        for argument in arguments {
            command_line.push(b' ' as u16);
            command_line.extend(quote_argument(OsStr::new(argument))?);
        }
        command_line.push(0);
        let environment = environment_block(environment)?;
        let stdin = inheritable(&File::open("NUL")?)?;
        let stdout = inheritable(log)?;
        let handles = [raw(&stdin), raw(&stdout)];
        let jobs = [raw(&self.0)];
        let attributes = Attributes::new()?;
        // Keep both arrays and the duplicated handles alive until CreateProcess
        // returns. Only stdio is inherited: never pass the job handle to a child.
        unsafe {
            UpdateProcThreadAttribute(
                attributes.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(handles.as_ptr().cast()),
                size_of_val(&handles),
                None,
                None,
            )?;
            UpdateProcThreadAttribute(
                attributes.ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(jobs.as_ptr().cast()),
                size_of_val(&jobs),
                None,
                None,
            )?;
        }
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = handles[0];
        startup.StartupInfo.hStdOutput = handles[1];
        startup.StartupInfo.hStdError = handles[1];
        startup.lpAttributeList = attributes.ptr();
        let mut information = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_UNICODE_ENVIRONMENT
                    | CREATE_NEW_PROCESS_GROUP
                    | CREATE_NO_WINDOW,
                Some(environment.as_ptr().cast()),
                PCWSTR(directory.as_ptr()),
                &startup.StartupInfo,
                &mut information,
            )
        }
        .with_context(|| format!("无法在服务进程组中启动 {}", executable.display()))?;
        let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess.0) };
        let _thread = unsafe { OwnedHandle::from_raw_handle(information.hThread.0) };
        Ok(ManagedChild {
            process,
            id: information.dwProcessId,
        })
    }

    pub fn stop(&self) -> anyhow::Result<()> {
        // Accounting reaches zero before the last process handles necessarily
        // become signaled. Retain descendant handles BEFORE terminating the job
        // and wait for their teardown as well (including pending pipe I/O).
        let processes = self.process_handles()?;
        unsafe { TerminateJobObject(raw(&self.0), 0) }.context("无法停止服务进程组")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        for process in processes {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(
                unsafe { WaitForSingleObject(raw(&process), remaining.as_millis() as u32) }
                    == WAIT_OBJECT_0,
                "等待服务进程结束超时"
            );
        }
        loop {
            let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
            unsafe {
                QueryInformationJobObject(
                    Some(raw(&self.0)),
                    JobObjectBasicAccountingInformation,
                    (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                    size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                    None,
                )?;
            }
            if info.ActiveProcesses == 0 {
                return Ok(());
            }
            ensure!(Instant::now() < deadline, "等待服务进程组退出超时");
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn process_handles(&self) -> anyhow::Result<Vec<OwnedHandle>> {
        let mut storage = vec![0_usize; 128];
        loop {
            let result = unsafe {
                QueryInformationJobObject(
                    Some(raw(&self.0)),
                    JobObjectBasicProcessIdList,
                    storage.as_mut_ptr().cast(),
                    size_of_val(storage.as_slice()) as u32,
                    None,
                )
            };
            if let Err(error) = result {
                if error.code() == ERROR_MORE_DATA.to_hresult() {
                    storage.resize(storage.len() * 2, 0);
                    continue;
                }
                return Err(error.into());
            }
            let list = unsafe { &*storage.as_ptr().cast::<JOBOBJECT_BASIC_PROCESS_ID_LIST>() };
            let ids = unsafe {
                std::slice::from_raw_parts(
                    list.ProcessIdList.as_ptr(),
                    list.NumberOfProcessIdsInList as usize,
                )
            };
            let mut handles = Vec::new();
            for &id in ids {
                let handle = match unsafe {
                    OpenProcess(
                        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                        false,
                        id as u32,
                    )
                } {
                    Ok(handle) => unsafe { OwnedHandle::from_raw_handle(handle.0) },
                    Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => continue,
                    Err(error) => return Err(error.into()),
                };
                let mut belongs = windows::core::BOOL::default();
                unsafe { IsProcessInJob(raw(&handle), Some(raw(&self.0)), &mut belongs) }?;
                if belongs.as_bool() {
                    handles.push(handle);
                }
            }
            return Ok(handles);
        }
    }
}

// OwnedHandle closes the non-inheritable job handle on every error path and on
// normal return. Windows also closes it if the supervisor is forcibly killed.
pub struct ManagedChild {
    process: OwnedHandle,
    id: u32,
}

/// Hold the verified process handle throughout shutdown; a recycled PID must
/// never make us wait for or terminate an unrelated application.
pub struct ExistingProcess {
    process: OwnedHandle,
    pub id: u32,
}

impl ExistingProcess {
    pub fn open(id: u32, expected: &Path) -> anyhow::Result<Option<Self>> {
        if id == 0 {
            return Ok(None);
        }
        let handle = match unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
                false,
                id,
            )
        } {
            Ok(handle) => handle,
            Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Ok(None),
            Err(error) => return Err(error).with_context(|| format!("无法检查进程 {id}")),
        };
        let process = Self {
            process: unsafe { OwnedHandle::from_raw_handle(handle.0) },
            id,
        };
        if process.wait(Duration::ZERO)? {
            return Ok(None);
        }
        let mut image = vec![0_u16; 32_768];
        let mut size = image.len() as u32;
        unsafe {
            QueryFullProcessImageNameW(
                raw(&process.process),
                PROCESS_NAME_FORMAT(0),
                PWSTR(image.as_mut_ptr()),
                &mut size,
            )
        }?;
        use std::os::windows::ffi::OsStringExt;
        let actual = std::path::PathBuf::from(OsString::from_wide(&image[..size as usize]));
        if actual.canonicalize()? != expected.canonicalize()? {
            return Ok(None);
        }
        Ok(Some(process))
    }

    pub fn wait(&self, timeout: Duration) -> anyhow::Result<bool> {
        match unsafe {
            WaitForSingleObject(
                raw(&self.process),
                timeout.as_millis().min(u32::MAX as u128 - 1) as u32,
            )
        } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(windows::core::Error::from_thread().into()),
        }
    }

    pub fn terminate(&self) -> anyhow::Result<()> {
        if !self.wait(Duration::ZERO)? {
            unsafe { TerminateProcess(raw(&self.process), 0) }?;
        }
        ensure!(
            self.wait(Duration::from_secs(10))?,
            "等待进程 {} 退出超时",
            self.id
        );
        Ok(())
    }
}

impl ManagedChild {
    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn try_wait(&self) -> anyhow::Result<Option<ExitStatus>> {
        match unsafe { WaitForSingleObject(raw(&self.process), 0) } {
            WAIT_TIMEOUT => Ok(None),
            WAIT_OBJECT_0 => {
                let mut code = 0;
                unsafe { GetExitCodeProcess(raw(&self.process), &mut code) }?;
                Ok(Some(ExitStatus::from_raw(code)))
            }
            _ => Err(windows::core::Error::from_thread().into()),
        }
    }
}

struct Attributes(Vec<usize>);

impl Attributes {
    fn new() -> anyhow::Result<Self> {
        let mut size = 0;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut size) };
        ensure!(size > 0, "无法确定服务进程属性大小");
        let mut storage = vec![0_usize; size.div_ceil(size_of::<usize>())];
        // Do not run DeleteProcThreadAttributeList unless initialization succeeds.
        unsafe {
            InitializeProcThreadAttributeList(
                Some(LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast())),
                2,
                None,
                &mut size,
            )?;
        }
        Ok(Self(storage))
    }

    fn ptr(&self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        LPPROC_THREAD_ATTRIBUTE_LIST(self.0.as_ptr().cast_mut().cast())
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.ptr()) };
    }
}

fn raw(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

fn inheritable(file: &File) -> anyhow::Result<OwnedHandle> {
    let mut duplicate = HANDLE::default();
    unsafe {
        DuplicateHandle(
            GetCurrentProcess(),
            HANDLE(file.as_raw_handle()),
            GetCurrentProcess(),
            &mut duplicate,
            0,
            true,
            DUPLICATE_SAME_ACCESS,
        )?;
        Ok(OwnedHandle::from_raw_handle(duplicate.0))
    }
}

fn wide(value: &OsStr) -> anyhow::Result<Vec<u16>> {
    let mut result: Vec<_> = value.encode_wide().collect();
    ensure!(!result.contains(&0), "进程参数包含 NUL 字符");
    result.push(0);
    Ok(result)
}

fn quote_argument(value: &OsStr) -> anyhow::Result<Vec<u16>> {
    let mut output = vec![b'"' as u16];
    let mut slashes = 0;
    for unit in value.encode_wide() {
        if unit == 0 {
            bail!("进程参数包含 NUL 字符");
        }
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        let is_quote = unit == b'"' as u16;
        output.extend(std::iter::repeat_n(
            b'\\' as u16,
            if is_quote { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        output.push(unit);
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
    Ok(output)
}

fn environment_block(overrides: &BTreeMap<&str, String>) -> anyhow::Result<Vec<u16>> {
    // Windows variable names are case-insensitive. Preserve inherited variables
    // (including =C: drive entries), replacing overrides without duplicate keys.
    let mut entries: BTreeMap<OsString, (OsString, OsString)> = std::env::vars_os()
        .map(|(key, value)| {
            (
                OsString::from(key.to_string_lossy().to_uppercase()),
                (key, value),
            )
        })
        .collect();
    for (key, value) in overrides {
        entries.insert(
            OsString::from(key.to_uppercase()),
            (OsString::from(key), OsString::from(value)),
        );
    }
    let mut block = Vec::new();
    for (_, (key, value)) in entries {
        let mut entry = key;
        entry.push("=");
        entry.push(value);
        block.extend(wide(&entry)?);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::{
        fs,
        path::PathBuf,
        process::{Child, Command, Stdio},
    };

    const FIXTURE: &str = "process_tree::tests::subprocess_fixture";

    pub(crate) struct TestDirectory(pub PathBuf);

    impl TestDirectory {
        pub(crate) fn new() -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "super-remote-process-test-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            // Only this test's uniquely created directory, never user runtime data.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(crate) struct ChildGuard(pub Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn fixture_args() -> Vec<String> {
        ["--ignored", "--exact", FIXTURE, "--nocapture"]
            .map(str::to_owned)
            .to_vec()
    }

    fn spawn_fixture(job: &ServiceJob, directory: &Path, mode: &str) -> ManagedChild {
        job.spawn(
            &std::env::current_exe().unwrap(),
            &fixture_args(),
            directory,
            &File::create(directory.join("fixture.log")).unwrap(),
            &BTreeMap::from([("SUPER_REMOTE_TEST_MODE", mode.into())]),
        )
        .unwrap()
    }

    fn detached_fixture(directory: &Path, mode: &str) -> ChildGuard {
        ChildGuard(
            Command::new(std::env::current_exe().unwrap())
                .args(fixture_args())
                .env("SUPER_REMOTE_TEST_MODE", mode)
                .current_dir(directory)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW.0)
                .spawn()
                .unwrap(),
        )
    }

    pub(crate) fn wait_until(mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !predicate() {
            assert!(Instant::now() < deadline, "fixture timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn wait_for_descendant(directory: &Path) -> ExistingProcess {
        let mut pid = None;
        wait_until(|| {
            pid = fs::read_to_string(directory.join("descendant.pid"))
                .ok()
                .and_then(|text| text.parse().ok());
            pid.is_some()
        });
        ExistingProcess::open(pid.unwrap(), &std::env::current_exe().unwrap())
            .unwrap()
            .unwrap()
    }

    pub(crate) fn start_test_stack(directory: &Path) -> (ChildGuard, ChildGuard) {
        for name in [
            "super-remote.exe",
            "remote-host.exe",
            "remote-signaling.exe",
            "remote-turn.exe",
            "remote-control-panel.exe",
        ] {
            let path = directory.join(name);
            if !path.is_file() {
                fs::copy(std::env::current_exe().unwrap(), path).unwrap();
            }
        }
        let spawn = |name: &str, mode: &str| {
            ChildGuard(
                Command::new(directory.join(name))
                    .args(fixture_args())
                    .env("SUPER_REMOTE_TEST_MODE", mode)
                    .current_dir(directory)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW.0)
                    .spawn()
                    .unwrap(),
            )
        };
        let panel = spawn("remote-control-panel.exe", "idle");
        let owner = spawn("super-remote.exe", "stack");
        wait_until(|| {
            let state: serde_json::Value = serde_json::from_slice(
                &fs::read(directory.join("status.json")).unwrap_or_default(),
            )
            .unwrap_or_default();
            state["launcher_pid"].as_u64() == Some(owner.0.id() as u64)
                && directory.join("descendant.pid").is_file()
        });
        (owner, panel)
    }

    #[test]
    fn stop_waits_for_services_and_descendants_without_killing_siblings() {
        let directory = TestDirectory::new();
        let sibling = detached_fixture(&directory.0, "idle");
        let job = ServiceJob::new().unwrap();
        let service = spawn_fixture(&job, &directory.0, "service");
        let descendant = wait_for_descendant(&directory.0);
        job.stop().unwrap();
        assert!(service.try_wait().unwrap().is_some());
        assert!(descendant.wait(Duration::from_secs(1)).unwrap());
        let sibling_process =
            ExistingProcess::open(sibling.0.id(), &std::env::current_exe().unwrap())
                .unwrap()
                .unwrap();
        assert!(!sibling_process.wait(Duration::ZERO).unwrap());
        // Repeated stops are safe, including an empty service group.
        job.stop().unwrap();
    }

    #[test]
    fn dropping_job_cleans_up_an_already_orphaned_descendant() {
        let directory = TestDirectory::new();
        let job = ServiceJob::new().unwrap();
        let service = spawn_fixture(&job, &directory.0, "orphan");
        let descendant = wait_for_descendant(&directory.0);
        wait_until(|| service.try_wait().unwrap().is_some());
        assert!(!descendant.wait(Duration::ZERO).unwrap());
        drop(job);
        assert!(descendant.wait(Duration::from_secs(3)).unwrap());
    }

    #[test]
    fn killing_supervisor_reclaims_the_entire_job() {
        let directory = TestDirectory::new();
        let mut owner = detached_fixture(&directory.0, "owner");
        let descendant = wait_for_descendant(&directory.0);
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        assert!(descendant.wait(Duration::from_secs(3)).unwrap());
    }

    #[test]
    fn spawn_preserves_unicode_quoting_environment_and_redirected_output() {
        let directory = TestDirectory::new();
        let job = ServiceJob::new().unwrap();
        let expected = [
            "",
            "含 空格的路径",
            "C:\\a b\\",
            "embedded\"quote",
            "\\\"quote\\",
        ];
        let mut args = fixture_args();
        for arg in expected {
            args.extend(["--skip".into(), arg.into()]);
        }
        // An empty --skip would skip the fixture itself. Use --test-threads for
        // the actual process and check the empty argument encoder separately.
        args.drain(4..6);
        assert_eq!(quote_argument(OsStr::new("")).unwrap(), vec![34, 34]);
        let child = job
            .spawn(
                &std::env::current_exe().unwrap(),
                &args,
                &directory.0,
                &File::create(directory.0.join("fixture.log")).unwrap(),
                &BTreeMap::from([
                    ("SUPER_REMOTE_TEST_MODE", "arguments".into()),
                    ("Super_Remote_Test_Value", "带空格的环境变量 ✅".into()),
                ]),
            )
            .unwrap();
        wait_until(|| child.try_wait().unwrap().is_some());
        assert!(child.try_wait().unwrap().unwrap().success());
        let actual: Vec<String> =
            serde_json::from_slice(&fs::read(directory.0.join("arguments.json")).unwrap()).unwrap();
        assert_eq!(&actual[1..], args);
        let log = fs::read_to_string(directory.0.join("fixture.log")).unwrap();
        assert!(log.contains("fixture stdout"));
        assert!(log.contains("fixture stderr"));
    }

    #[test]
    fn process_identity_rejects_an_unrelated_executable() {
        let expected = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap()
            .join("System32\\cmd.exe");
        assert!(
            ExistingProcess::open(std::process::id(), &expected)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    #[ignore = "subprocess fixture, invoked only by the lifecycle tests"]
    fn subprocess_fixture() {
        let mode = std::env::var("SUPER_REMOTE_TEST_MODE").unwrap();
        if mode == "stack" {
            let directory = std::env::current_dir().unwrap();
            let job = ServiceJob::new().unwrap();
            let spawn = |name: &str, mode: &str| {
                job.spawn(
                    &directory.join(name),
                    &fixture_args(),
                    &directory,
                    &File::create(directory.join(format!("{name}.log"))).unwrap(),
                    &BTreeMap::from([("SUPER_REMOTE_TEST_MODE", mode.into())]),
                )
                .unwrap()
            };
            let host = spawn("remote-host.exe", "service");
            let signaling = spawn("remote-signaling.exe", "idle");
            let turn = spawn("remote-turn.exe", "idle");
            fs::write(
                "supervisor-state.json",
                serde_json::to_vec(&serde_json::json!({
                    "launcher_pid": std::process::id(), "process_tree_managed": true,
                }))
                .unwrap(),
            )
            .unwrap();
            fs::write("status.json", serde_json::to_vec(&serde_json::json!({
                "launcher_pid": std::process::id(), "host_pid": host.id(), "signaling_pid": signaling.id(), "turn_pid": turn.id(),
            })).unwrap()).unwrap();
            let marker = directory.join(format!("shutdown-{}.requested", std::process::id()));
            while !marker.is_file() {
                thread::sleep(Duration::from_millis(10));
            }
            job.stop().unwrap();
            return;
        }
        if mode == "arguments" {
            assert_eq!(
                std::env::var("SUPER_REMOTE_TEST_VALUE").unwrap(),
                "带空格的环境变量 ✅"
            );
            assert!(std::env::var_os("SystemRoot").is_some());
            fs::write(
                "arguments.json",
                serde_json::to_vec(&std::env::args().collect::<Vec<_>>()).unwrap(),
            )
            .unwrap();
            println!("fixture stdout");
            eprintln!("fixture stderr");
            return;
        }
        let _job = if mode == "owner" {
            let job = ServiceJob::new().unwrap();
            let _child = spawn_fixture(&job, &std::env::current_dir().unwrap(), "service");
            Some(job)
        } else {
            None
        };
        if mode == "service" || mode == "orphan" {
            let mut child = detached_fixture(&std::env::current_dir().unwrap(), "idle");
            fs::write("descendant.pid", child.0.id().to_string()).unwrap();
            if mode == "orphan" {
                // Deliberately bypass destructors to simulate Host crashing.
                std::process::exit(0);
            }
            child.0.wait().unwrap();
            return;
        }
        // The owning test always kills this hidden process (including on panic).
        loop {
            thread::sleep(Duration::from_millis(100));
        }
    }
}
