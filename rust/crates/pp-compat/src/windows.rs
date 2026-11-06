//! Windows supervision. A Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
//! takes the place of the Unix process group and the parent-death watcher: the
//! child starts suspended, joins the job before it runs, and every process it
//! starts joins the job too. Only the core holds the job handle, so when the
//! core exits or is killed the kernel closes the handle and terminates every
//! process in the job. The private channel is a named pipe that the core
//! creates as the first instance, with a DACL that grants only the current
//! user and `PIPE_REJECT_REMOTE_CLIENTS`; the child dials in.

use crate::windows_security::{CurrentUser, PrivateDescriptor};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, LazyLock, Mutex},
};
use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    process::{Child, Command},
};
use windows_sys::Win32::{
    Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT},
    Security::{Authorization::SE_FILE_OBJECT, SECURITY_ATTRIBUTES},
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, READ_CONTROL,
    },
    System::{
        Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
        },
        JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Threading::{
            CREATE_SUSPENDED, OpenProcess, OpenThread, PROCESS_SYNCHRONIZE, ResumeThread,
            THREAD_SUSPEND_RESUME, WaitForSingleObject,
        },
    },
};

/// Environment names the child inherits from the core. Node needs
/// `SystemRoot` to start its networking and crypto.
pub(crate) const INHERITED_ENV: &[&str] = &[
    "PATH",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERPROFILE",
    "LOCALAPPDATA",
    "APPDATA",
    "TZ",
];

/// Exit code of every process the core terminates through its job.
pub(crate) const JOB_KILL_EXIT_CODE: u32 = 0x5050_4b4c;

fn check(ok: i32) -> std::io::Result<()> {
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

struct Job(OwnedHandle);

impl Job {
    fn create() -> std::io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = Self(unsafe { OwnedHandle::from_raw_handle(handle as _) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        check(unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        })?;
        Ok(job)
    }

    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }

    /// Processes in the job that have not finished terminating.
    fn active(&self) -> u32 {
        let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let queried = unsafe {
            QueryInformationJobObject(
                self.raw(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if queried == 0 {
            return u32::MAX;
        }
        accounting.ActiveProcesses
    }
}

/// Jobs of running children by leader PID, so the PID-based group helpers in
/// `drain` reach the job. An entry lives as long as its [`ParentDeathGuard`].
static JOBS: LazyLock<Mutex<HashMap<u32, Arc<Job>>>> = LazyLock::new(Default::default);

fn job_of(pid: u32) -> Option<Arc<Job>> {
    JOBS.lock().ok()?.get(&pid).cloned()
}

/// Owns the child's job. Dropping it closes the handle, and the kernel then
/// terminates every process still in the job.
pub(crate) struct ParentDeathGuard {
    job: Arc<Job>,
    pid: Option<u32>,
}

impl ParentDeathGuard {
    /// Supervision finished cleanly; close the job.
    pub(crate) fn release(&mut self) {
        if let Some(pid) = self.pid.take()
            && let Ok(mut jobs) = JOBS.lock()
        {
            jobs.remove(&pid);
        }
    }
}

impl Drop for ParentDeathGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// The child has nothing to release after spawn; the core keeps the storage lock.
pub(crate) struct ChildSetup;

/// Start the child suspended inside a new kill-on-close job. The storage lock
/// stays with the core: a Windows byte-range lock belongs to the process that
/// took it, and the job guarantees the child never outlives the core.
pub(crate) fn configure_child(
    command: &mut Command,
    _storage_lock: &File,
) -> std::io::Result<(ChildSetup, ParentDeathGuard)> {
    let job = Arc::new(Job::create()?);
    command.creation_flags(CREATE_SUSPENDED);
    Ok((ChildSetup, ParentDeathGuard { job, pid: None }))
}

/// Put the suspended child in its job, then let it run.
pub(crate) fn adopt(
    _setup: ChildSetup,
    guard: &mut ParentDeathGuard,
    child: &Child,
) -> std::io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("Child exited before joining its job"))?;
    let process = child
        .raw_handle()
        .ok_or_else(|| std::io::Error::other("Child exited before joining its job"))?;
    check(unsafe { AssignProcessToJobObject(guard.job.raw(), process as HANDLE) })?;
    JOBS.lock()
        .map_err(|_| std::io::Error::other("Job registry poisoned"))?
        .insert(pid, Arc::clone(&guard.job));
    guard.pid = Some(pid);
    resume_threads(pid)
}

fn resume_threads(pid: u32) -> std::io::Result<()> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot as _) };
    let mut entry = THREADENTRY32 {
        dwSize: size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    let mut resumed = 0;
    let mut more = unsafe { Thread32First(snapshot.as_raw_handle() as HANDLE, &mut entry) };
    while more != 0 {
        if entry.th32OwnerProcessID == pid {
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if thread.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let count = unsafe { ResumeThread(thread) };
            unsafe { CloseHandle(thread) };
            if count == u32::MAX {
                return Err(std::io::Error::last_os_error());
            }
            resumed += 1;
        }
        more = unsafe { Thread32Next(snapshot.as_raw_handle() as HANDLE, &mut entry) };
    }
    if resumed == 0 {
        return Err(std::io::Error::other("Suspended child has no thread"));
    }
    Ok(())
}

/// Windows has no stop signal for a console-less child. The child stops when
/// its stdin closes, which planned shutdown does before this call.
pub(crate) fn request_group_stop(_pid: u32) -> bool {
    false
}

/// Terminate every process in the child's job. True when one was running.
pub(crate) fn kill_group(pid: u32) -> bool {
    let Some(job) = job_of(pid) else {
        return false;
    };
    job.active() > 0 && unsafe { TerminateJobObject(job.raw(), JOB_KILL_EXIT_CODE) } != 0
}

/// A process of the child's job is still running. After the job is released,
/// only the leader can be asked about, and closing the job terminated the rest.
pub(crate) fn group_alive(pid: u32) -> bool {
    match job_of(pid) {
        Some(job) => job.active() > 0,
        None => process_alive(pid),
    }
}

pub(crate) fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if process.is_null() {
        return false;
    }
    let running = unsafe { WaitForSingleObject(process, 0) } == WAIT_TIMEOUT;
    unsafe { CloseHandle(process) };
    running
}

/// The child ended because the core terminated its job.
pub(crate) fn stopped_by_parent(status: &ExitStatus) -> bool {
    status.code() == Some(JOB_KILL_EXIT_CODE as i32)
}

pub(crate) fn signal(_status: &ExitStatus) -> Option<i32> {
    None
}

/// Open `path` for reading as itself: a reparse point is opened, not followed,
/// and [`private_file`] refuses it.
pub(crate) fn open_nofollow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .access_mode(FILE_GENERIC_READ | READ_CONTROL)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

/// A regular file, not a reparse point, owned by this user and open to no
/// other account except SYSTEM and Administrators.
pub(crate) fn private_file(file: &File) -> std::io::Result<bool> {
    let metadata = file.metadata()?;
    Ok(metadata.is_file()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && CurrentUser::query()?.owns_privately(file, SE_FILE_OBJECT)?)
}

/// `\\.\pipe\printpartner-compat-<generation>`. The core always holds one
/// listening instance; each connect takes it and creates the next.
pub(crate) struct Channel {
    name: PathBuf,
    descriptor: Option<PrivateDescriptor>,
    listener: tokio::sync::Mutex<Option<NamedPipeServer>>,
}

pub(crate) type Stream = NamedPipeServer;

fn create_instance(
    name: &Path,
    first: bool,
    descriptor: &PrivateDescriptor,
) -> std::io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            )
    }
}

impl Channel {
    pub(crate) fn bind(_runtime_dir: &Path, generation: &str) -> std::io::Result<Self> {
        let name = PathBuf::from(format!(r"\\.\pipe\printpartner-compat-{generation}"));
        let descriptor = CurrentUser::query()?.private_descriptor(false)?;
        let listener = create_instance(&name, true, &descriptor)?;
        Ok(Self {
            name,
            descriptor: Some(descriptor),
            listener: tokio::sync::Mutex::new(Some(listener)),
        })
    }

    /// No pipe exists; every connect fails.
    pub(crate) fn unbound(name: PathBuf) -> Self {
        Self {
            name,
            descriptor: None,
            listener: tokio::sync::Mutex::new(None),
        }
    }

    /// The address the child receives as `socket_path` and dials.
    pub(crate) fn name(&self) -> &Path {
        &self.name
    }

    pub(crate) async fn connect(&self) -> std::io::Result<Stream> {
        let mut listener = self.listener.lock().await;
        let (Some(server), Some(descriptor)) = (listener.as_ref(), self.descriptor.as_ref()) else {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        };
        server.connect().await?;
        let next = create_instance(&self.name, false, descriptor)?;
        listener
            .replace(next)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    pub(crate) async fn close(&self) -> std::io::Result<()> {
        self.listener.lock().await.take();
        Ok(())
    }
}

/// The interpreter for test child scripts: Node with `-e`, since Windows has no
/// `/bin/sh`. Scripts for it are JavaScript.
#[cfg(test)]
pub(crate) fn test_shell(script: &str) -> Command {
    let mut command = Command::new(crate::tests::node_binary());
    command.arg("-e").arg(script).env_clear().envs(
        ["SystemRoot", "PATH"]
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (*name, value))),
    );
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::{JobObjects::IsProcessInJob, Threading::PROCESS_QUERY_LIMITED_INFORMATION},
    };

    const JOB_OWNER_ROOT: &str = "PP_COMPAT_JOB_OWNER_ROOT";

    /// Spawns a detached grandchild, which tries to leave the job, and records
    /// its PID beside the data directory.
    const SPAWN_GRANDCHILD: &str = concat!(
        "const {spawn}=require('node:child_process'); const fs=require('node:fs'); ",
        "const path=require('node:path'); let input=''; ",
        "process.stdin.on('data', chunk => { input += chunk; const end = input.indexOf('\\n'); ",
        "if (end < 0) return; process.stdin.pause(); ",
        "const setup = JSON.parse(input.slice(0, end)); ",
        "const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 1e9)'], ",
        "{stdio: 'ignore', detached: true}); child.unref(); ",
        "const file = path.join(setup.data_dir, '..', 'grandchild'); ",
        "fs.writeFileSync(file + '.tmp', String(child.pid)); fs.renameSync(file + '.tmp', file); }); ",
        "setInterval(() => {}, 1e9);",
    );

    /// The core for the job test: a supervisor whose child has started a
    /// grandchild. The test kills this process.
    #[tokio::test]
    async fn job_owner_fixture() {
        let Some(root) = std::env::var_os(JOB_OWNER_ROOT) else {
            return;
        };
        let root = PathBuf::from(root);
        let supervisor = crate::Supervisor::launch_shell(
            crate::ShellSpec {
                script: SPAWN_GRANDCHILD,
                bundle: crate::Bundle {
                    node: crate::tests::node_binary(),
                    entry: root.join("unused.js"),
                    web_root: root.clone(),
                    runtime_version: "job-test".into(),
                    commit: "job-test".into(),
                },
                data_dir: root.join("data"),
                runtime_dir: root.join("run"),
                pidfile: root.join("leader"),
                setup_padding: String::new(),
                port: 0,
            },
            crate::SupervisorConfig::default(),
        )
        .unwrap();
        std::future::pending::<()>().await;
        drop(supervisor);
    }

    fn wait_for(path: &Path, owner: &mut std::process::Child) -> u32 {
        let started = std::time::Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(path)
                && let Some(pid) = text
                    .lines()
                    .next()
                    .and_then(|line| line.trim().parse().ok())
            {
                return pid;
            }
            if let Some(status) = owner.try_wait().unwrap() {
                panic!("job owner exited early: {status}");
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "{} never appeared",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn open_process(pid: u32) -> OwnedHandle {
        let process = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            )
        };
        assert!(!process.is_null(), "{}", std::io::Error::last_os_error());
        unsafe { OwnedHandle::from_raw_handle(process as _) }
    }

    fn in_job(process: &OwnedHandle) -> bool {
        let mut member = 0;
        check(unsafe {
            IsProcessInJob(
                process.as_raw_handle() as HANDLE,
                std::ptr::null_mut(),
                &mut member,
            )
        })
        .unwrap();
        member != 0
    }

    /// The no-orphan guarantee (T-3) on Windows: killing the core outright,
    /// which runs no cleanup, terminates its child and the grandchild that
    /// detached itself, because closing the core's job handle kills the job.
    #[test]
    fn ticket_t_3_drain_job_object_kills_children_when_core_is_killed() {
        let root = std::env::temp_dir().join(format!(
            "pp-job-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(root.join("run")).unwrap();
        let mut owner = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows::tests::job_owner_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(JOB_OWNER_ROOT, &root)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let leader = open_process(wait_for(&root.join("leader"), &mut owner));
        let grandchild = open_process(wait_for(&root.join("grandchild"), &mut owner));
        for process in [&leader, &grandchild] {
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, 0) },
                WAIT_TIMEOUT
            );
            assert!(in_job(process));
        }
        owner.kill().unwrap();
        owner.wait().unwrap();
        for (name, process) in [("child", &leader), ("grandchild", &grandchild)] {
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, 10_000) },
                WAIT_OBJECT_0,
                "{name} outlived the killed core"
            );
        }
        drop((leader, grandchild));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn private_pipe_is_first_instance_owner_only_and_serves_each_connect() {
        let generation = format!("test-{}", rand::random::<u64>());
        let runtime = std::env::temp_dir();
        let channel = Channel::bind(&runtime, &generation).unwrap();
        assert!(Channel::bind(&runtime, &generation).is_err());
        for _ in 0..2 {
            let name = channel.name().to_path_buf();
            let client = tokio::task::spawn_blocking(move || {
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .access_mode(FILE_GENERIC_READ | READ_CONTROL | 0x4000_0000)
                    .open(name)
            });
            let mut server = channel.connect().await.unwrap();
            let mut client = client.await.unwrap().unwrap();
            assert!(
                CurrentUser::query()
                    .unwrap()
                    .owns_privately(&client, SE_FILE_OBJECT)
                    .unwrap()
            );
            use std::io::{Read, Write};
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            server.write_all(b"ping").await.unwrap();
            let mut received = [0; 4];
            client.read_exact(&mut received).unwrap();
            assert_eq!(&received, b"ping");
            client.write_all(b"pong").unwrap();
            server.read_exact(&mut received).await.unwrap();
            assert_eq!(&received, b"pong");
        }
        channel.close().await.unwrap();
        assert!(channel.connect().await.is_err());
    }
}
