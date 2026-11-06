//! Windows supervision. A Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
//! takes the place of the Unix process group and the parent-death watcher: the
//! child is created inside the job (`PROC_THREAD_ATTRIBUTE_JOB_LIST`), so there
//! is no moment it exists outside it, and every process it starts joins the
//! job too. Only the core holds the job handle, so when the
//! core exits or is killed the kernel closes the handle and terminates every
//! process in the job. The private channel is a named pipe that the core
//! creates as the first instance, with a DACL that grants only the current
//! user and `PIPE_REJECT_REMOTE_CLIENTS`; the child dials in.

use crate::windows_security::{CurrentUser, PrivateDescriptor};
use std::{
    collections::HashMap,
    ffi::{OsStr, c_void},
    fs::{File, OpenOptions},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use tokio::{
    net::windows::named_pipe::{NamedPipeServer, ServerOptions},
    process::{ChildStderr, ChildStdin, ChildStdout, Command},
    sync::watch,
};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_NOT_CONNECTED, HANDLE,
        HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation, WAIT_OBJECT_0,
        WAIT_TIMEOUT,
    },
    Security::{Authorization::SE_FILE_OBJECT, SECURITY_ATTRIBUTES},
    Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_FIRST_PIPE_INSTANCE,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, READ_CONTROL,
    },
    System::{
        IO::{CreateIoCompletionPort, GetQueuedCompletionStatus},
        JobObjects::{
            CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_ASSOCIATE_COMPLETION_PORT, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectAssociateCompletionPortInformation,
            JobObjectBasicAccountingInformation, JobObjectExtendedLimitInformation,
            QueryInformationJobObject, SetInformationJobObject, TerminateJobObject,
        },
        Pipes::{
            CreateNamedPipeW, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, PeekNamedPipe,
        },
        SystemServices::JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO,
        Threading::{
            CREATE_NO_WINDOW, CREATE_UNICODE_ENVIRONMENT, CreateProcessW,
            DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess,
            INFINITE, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST, PROCESS_INFORMATION,
            PROCESS_SYNCHRONIZE, RegisterWaitForSingleObject, STARTF_USESTDHANDLES, STARTUPINFOEXW,
            TerminateProcess, UnregisterWaitEx, UpdateProcThreadAttribute, WT_EXECUTEONLYONCE,
            WaitForSingleObject,
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

/// A Job Object and the completion port that receives its notifications.
struct Job {
    handle: OwnedHandle,
    port: OwnedHandle,
}

impl Job {
    fn create() -> std::io::Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let handle = unsafe { OwnedHandle::from_raw_handle(handle as _) };
        let port =
            unsafe { CreateIoCompletionPort(INVALID_HANDLE_VALUE, std::ptr::null_mut(), 0, 1) };
        if port.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let job = Self {
            handle,
            port: unsafe { OwnedHandle::from_raw_handle(port as _) },
        };
        let association = JOBOBJECT_ASSOCIATE_COMPLETION_PORT {
            CompletionKey: std::ptr::null_mut(),
            CompletionPort: job.port.as_raw_handle() as HANDLE,
        };
        check(unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectAssociateCompletionPortInformation,
                (&association as *const JOBOBJECT_ASSOCIATE_COMPLETION_PORT).cast(),
                size_of::<JOBOBJECT_ASSOCIATE_COMPLETION_PORT>() as u32,
            )
        })?;
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
        self.handle.as_raw_handle() as HANDLE
    }

    /// Block until the job has no active process, or until `bound` passes.
    /// The kernel posts `JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO` to the port when
    /// the last process starts terminating. Delivery of job messages is not
    /// guaranteed, so a lost message only costs the bound.
    fn wait_empty(&self, bound: Duration) {
        let deadline = std::time::Instant::now() + bound;
        while self.active() > 0 {
            let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) else {
                return;
            };
            let mut message = 0;
            let mut key = 0;
            let mut overlapped = std::ptr::null_mut();
            let dequeued = unsafe {
                GetQueuedCompletionStatus(
                    self.port.as_raw_handle() as HANDLE,
                    &mut message,
                    &mut key,
                    &mut overlapped,
                    u32::try_from(left.as_millis()).unwrap_or(u32::MAX).max(1),
                )
            };
            if dequeued == 0 || message == JOB_OBJECT_MSG_ACTIVE_PROCESS_ZERO {
                return;
            }
        }
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

/// The job the child is created in. The core keeps the storage lock.
pub(crate) struct ChildSetup(Arc<Job>);

/// Prepare a new kill-on-close job for the child. The storage lock stays with
/// the core: a Windows byte-range lock belongs to the process that took it,
/// and the job guarantees the child never outlives the core.
pub(crate) fn configure_child(
    _command: &mut Command,
    _storage_lock: &File,
) -> std::io::Result<(ChildSetup, ParentDeathGuard)> {
    let job = Arc::new(Job::create()?);
    Ok((
        ChildSetup(Arc::clone(&job)),
        ParentDeathGuard { job, pid: None },
    ))
}

/// Register the running child under its job for the PID-based group helpers.
pub(crate) fn adopt(
    _setup: ChildSetup,
    guard: &mut ParentDeathGuard,
    child: &Child,
) -> std::io::Result<()> {
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("Child exited before adoption"))?;
    JOBS.lock()
        .map_err(|_| std::io::Error::other("Job registry poisoned"))?
        .insert(pid, Arc::clone(&guard.job));
    guard.pid = Some(pid);
    Ok(())
}

/// A child created by [`spawn`], with the part of `tokio::process::Child` the
/// supervisor uses. Dropping it before it is reaped terminates it.
pub struct Child {
    pub(crate) stdin: Option<ChildStdin>,
    pub(crate) stdout: Option<ChildStdout>,
    pub(crate) stderr: Option<ChildStderr>,
    pid: u32,
    status: Option<ExitStatus>,
    exited: watch::Receiver<bool>,
    _wait: ExitWait,
    process: OwnedHandle,
}

impl Child {
    pub(crate) fn id(&self) -> Option<u32> {
        self.status.is_none().then_some(self.pid)
    }

    fn raw(&self) -> HANDLE {
        self.process.as_raw_handle() as HANDLE
    }

    pub(crate) fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
        if self.status.is_none() && unsafe { WaitForSingleObject(self.raw(), 0) } == WAIT_OBJECT_0 {
            let mut code = 0;
            check(unsafe { GetExitCodeProcess(self.raw(), &mut code) })?;
            self.status = Some(ExitStatus::from_raw(code));
        }
        Ok(self.status)
    }

    pub(crate) async fn wait(&mut self) -> std::io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            self.exited
                .wait_for(|exited| *exited)
                .await
                .map_err(|_| std::io::Error::other("Child exit watch closed"))?;
        }
    }

    pub(crate) fn start_kill(&mut self) -> std::io::Result<()> {
        if self.try_wait()?.is_none() {
            check(unsafe { TerminateProcess(self.raw(), JOB_KILL_EXIT_CODE) })?;
        }
        Ok(())
    }

    pub(crate) async fn kill(&mut self) -> std::io::Result<()> {
        self.start_kill()?;
        self.wait().await.map(drop)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if self.status.is_none() {
            unsafe { TerminateProcess(self.raw(), JOB_KILL_EXIT_CODE) };
        }
    }
}

/// A thread-pool wait that marks the process exited.
struct ExitWait {
    handle: HANDLE,
    sender: *const watch::Sender<bool>,
}

unsafe extern "system" fn process_exited(context: *mut c_void, _timed_out: bool) {
    let sender = unsafe { &*context.cast::<watch::Sender<bool>>() };
    sender.send_replace(true);
}

impl ExitWait {
    fn register(process: HANDLE) -> std::io::Result<(Self, watch::Receiver<bool>)> {
        let (sender, receiver) = watch::channel(false);
        let sender = Box::into_raw(Box::new(sender));
        let mut handle = std::ptr::null_mut();
        let registered = unsafe {
            RegisterWaitForSingleObject(
                &mut handle,
                process,
                Some(process_exited),
                sender.cast(),
                INFINITE,
                WT_EXECUTEONLYONCE,
            )
        };
        if registered == 0 {
            let error = std::io::Error::last_os_error();
            drop(unsafe { Box::from_raw(sender) });
            return Err(error);
        }
        Ok((Self { handle, sender }, receiver))
    }
}

impl Drop for ExitWait {
    fn drop(&mut self) {
        unsafe {
            UnregisterWaitEx(self.handle, INVALID_HANDLE_VALUE);
            drop(Box::from_raw(self.sender.cast_mut()));
        }
    }
}

unsafe impl Send for ExitWait {}
unsafe impl Sync for ExitWait {}

fn wide(text: &OsStr) -> Vec<u16> {
    text.encode_wide().chain([0]).collect()
}

/// Append `argument` to a command line the way the C runtime splits it.
fn push_argument(line: &mut Vec<u16>, argument: &OsStr) {
    let units: Vec<u16> = argument.encode_wide().collect();
    let plain = !units.is_empty()
        && !units.iter().any(|&unit| {
            unit == u16::from(b' ') || unit == u16::from(b'\t') || unit == u16::from(b'"')
        });
    if !line.is_empty() {
        line.push(u16::from(b' '));
    }
    if plain {
        line.extend(units);
        return;
    }
    line.push(u16::from(b'"'));
    let mut backslashes = 0;
    for unit in units {
        if unit == u16::from(b'\\') {
            backslashes += 1;
        } else {
            if unit == u16::from(b'"') {
                line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes + 1));
            }
            backslashes = 0;
        }
        line.push(unit);
    }
    line.extend(std::iter::repeat_n(u16::from(b'\\'), backslashes));
    line.push(u16::from(b'"'));
}

/// One end of a pipe for the child (synchronous, made inheritable only while
/// the child is created) and the core's overlapped end. The pipe carries the
/// HMAC key, so only the current user may open it.
fn pipe(child_reads: bool) -> std::io::Result<(OwnedHandle, OwnedHandle)> {
    let name = wide(OsStr::new(&format!(
        r"\\.\pipe\printpartner-stdio-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    )));
    let descriptor = CurrentUser::query()?.private_descriptor(false)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    let direction = if child_reads {
        PIPE_ACCESS_OUTBOUND
    } else {
        PIPE_ACCESS_INBOUND
    };
    let ours = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            direction | FILE_FLAG_FIRST_PIPE_INSTANCE | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            1,
            4096,
            4096,
            0,
            &attributes,
        )
    };
    if ours == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    let ours = unsafe { OwnedHandle::from_raw_handle(ours as _) };
    let access = if child_reads {
        FILE_GENERIC_READ
    } else {
        FILE_GENERIC_WRITE
    };
    let theirs = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            0,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if theirs == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error());
    }
    Ok((ours, unsafe { OwnedHandle::from_raw_handle(theirs as _) }))
}

/// Create `command` inside the job of `setup`, with piped stdin, stdout and
/// stderr. The child's environment is exactly the variables `command` sets,
/// as every caller starts from `env_clear`. It inherits only its three pipe
/// ends and opens no console window.
pub(crate) fn spawn(command: &mut Command, setup: &ChildSetup) -> std::io::Result<Child> {
    let command = command.as_std();
    let program = Path::new(command.get_program());
    if !program.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Child program must be an absolute path",
        ));
    }
    let mut line = Vec::new();
    push_argument(&mut line, program.as_os_str());
    for argument in command.get_args() {
        push_argument(&mut line, argument);
    }
    line.push(0);
    let mut variables: Vec<(&OsStr, &OsStr)> = command
        .get_envs()
        .filter_map(|(name, value)| value.map(|value| (name, value)))
        .collect();
    variables.sort_by_key(|(name, _)| name.to_ascii_uppercase());
    let mut environment = Vec::new();
    for (name, value) in variables {
        environment.extend(name.encode_wide());
        environment.push(u16::from(b'='));
        environment.extend(value.encode_wide());
        environment.push(0);
    }
    environment.extend([0, 0]);
    let directory = command
        .get_current_dir()
        .map(|directory| wide(directory.as_os_str()));
    let (stdin, child_stdin) = pipe(true)?;
    let (stdout, child_stdout) = pipe(false)?;
    let (stderr, child_stderr) = pipe(false)?;
    let inherited = [
        child_stdin.as_raw_handle() as HANDLE,
        child_stdout.as_raw_handle() as HANDLE,
        child_stderr.as_raw_handle() as HANDLE,
    ];
    let job = setup.0.raw();
    let mut size = 0;
    unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &mut size) };
    let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
    let list: LPPROC_THREAD_ATTRIBUTE_LIST = storage.as_mut_ptr().cast();
    check(unsafe { InitializeProcThreadAttributeList(list, 2, 0, &mut size) })?;
    let created = (|| {
        check(unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                (&job as *const HANDLE).cast(),
                size_of::<HANDLE>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        })?;
        check(unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                inherited.as_ptr().cast(),
                size_of_val(&inherited),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        })?;
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = inherited[0];
        startup.StartupInfo.hStdOutput = inherited[1];
        startup.StartupInfo.hStdError = inherited[2];
        startup.lpAttributeList = list;
        let mut information = PROCESS_INFORMATION::default();
        let program = wide(program.as_os_str());
        let _spawning = crate::SPAWN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let created = inherited
            .iter()
            .try_for_each(|&handle| {
                check(unsafe {
                    SetHandleInformation(handle, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT)
                })
            })
            .and_then(|()| {
                check(unsafe {
                    CreateProcessW(
                        program.as_ptr(),
                        line.as_mut_ptr(),
                        std::ptr::null(),
                        std::ptr::null(),
                        1,
                        CREATE_UNICODE_ENVIRONMENT
                            | EXTENDED_STARTUPINFO_PRESENT
                            | CREATE_NO_WINDOW,
                        environment.as_ptr().cast(),
                        directory
                            .as_ref()
                            .map_or(std::ptr::null(), |directory| directory.as_ptr()),
                        &startup.StartupInfo,
                        &mut information,
                    )
                })
            });
        for handle in inherited {
            unsafe { SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0) };
        }
        created.map(|()| information)
    })();
    unsafe { DeleteProcThreadAttributeList(list) };
    let information = created?;
    drop((child_stdin, child_stdout, child_stderr));
    unsafe { CloseHandle(information.hThread) };
    let process = unsafe { OwnedHandle::from_raw_handle(information.hProcess as _) };
    let (wait, exited) = match ExitWait::register(information.hProcess) {
        Ok(registered) => registered,
        Err(error) => {
            unsafe { TerminateProcess(information.hProcess, 1) };
            return Err(error);
        }
    };
    Ok(Child {
        stdin: Some(ChildStdin::from_std(std::process::ChildStdin::from(stdin))?),
        stdout: Some(ChildStdout::from_std(std::process::ChildStdout::from(
            stdout,
        ))?),
        stderr: Some(ChildStderr::from_std(std::process::ChildStderr::from(
            stderr,
        ))?),
        pid: information.dwProcessId,
        status: None,
        exited,
        _wait: wait,
        process,
    })
}

/// Windows has no stop signal for a console-less child. The child stops when
/// its stdin closes, which planned shutdown does before this call.
pub(crate) fn request_group_stop(_pid: u32) -> bool {
    false
}

/// Wait up to `bound` for the child's job to empty on its own after its leader
/// exited. Node runs with `CREATE_NO_WINDOW`, so the job also holds the
/// conhost.exe that serves Node's hidden console, and conhost exits only a
/// moment after Node. Without this wait, the reap that follows a clean exit
/// can find conhost still active, terminate the job, and report the exit as
/// forced.
pub(crate) async fn settle_group(pid: u32, bound: Duration) {
    let Some(job) = job_of(pid) else {
        return;
    };
    let _ = tokio::task::spawn_blocking(move || job.wait_empty(bound)).await;
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
/// other account except SYSTEM and Administrators, which the profile folder
/// grants through inherited ACEs and which can read any file anyway.
pub(crate) fn private_file(file: &File) -> std::io::Result<bool> {
    let metadata = file.metadata()?;
    Ok(metadata.is_file()
        && metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && CurrentUser::query()?.owns_privately_or_root(file, SE_FILE_OBJECT)?)
}

/// `\\.\pipe\printpartner-compat-<generation>`. The core always holds one
/// listening instance; each connect takes it and creates the next.
pub(crate) struct Channel {
    name: PathBuf,
    descriptor: Option<PrivateDescriptor>,
    listener: tokio::sync::Mutex<Option<NamedPipeServer>>,
}

pub(crate) type Stream = NamedPipeServer;

/// How long a request waits for the child's queued dial.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Whether the client of a connected instance is still attached: peeking a
/// pipe whose client closed fails with a broken or disconnected pipe.
fn client_attached(server: &NamedPipeServer) -> std::io::Result<bool> {
    let mut available = 0;
    let peeked = unsafe {
        PeekNamedPipe(
            server.as_raw_handle() as HANDLE,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if peeked != 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error().map(|code| code as u32) {
        Some(ERROR_BROKEN_PIPE | ERROR_PIPE_NOT_CONNECTED | ERROR_NO_DATA) => Ok(false),
        _ => Err(error),
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

    /// Take the connection the child dialed into the listening instance and
    /// create the next instance. A dial that closed before it was taken leaves
    /// the instance dead, and `ConnectNamedPipe`'s `ERROR_NO_DATA` reaches here
    /// as a success, so [`client_attached`] checks the client is still there; a
    /// dead instance is replaced and the wait goes on for the child's redial. A
    /// client that closes right after that check fails this one request. The
    /// child keeps a dial queued, so a wait past [`CONNECT_TIMEOUT`] means it
    /// stopped dialing and fails the request instead of holding it forever. If
    /// the next instance cannot be created, the connected one is still returned
    /// and later connects fail.
    pub(crate) async fn connect(&self) -> std::io::Result<Stream> {
        let mut listener = self.listener.lock().await;
        let Some(descriptor) = self.descriptor.as_ref() else {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        };
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            loop {
                let server = listener
                    .as_ref()
                    .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
                if server.connect().await.is_ok() && client_attached(server)? {
                    return Ok::<(), std::io::Error>(());
                }
                listener.replace(create_instance(&self.name, false, descriptor)?);
            }
        })
        .await
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::TimedOut))??;
        let connected = match create_instance(&self.name, false, descriptor) {
            Ok(next) => listener.replace(next),
            Err(_) => listener.take(),
        };
        connected.ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
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

/// Fixtures for the supervision tests in `lib.rs`.
#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        Channel, Child, CloseHandle, JOB_KILL_EXIT_CODE, OpenProcess, ParentDeathGuard,
        TerminateProcess, adopt, configure_child, spawn,
    };
    use std::{
        fs::{File, OpenOptions},
        os::windows::process::ExitStatusExt,
        path::Path,
        process::ExitStatus,
    };
    use tokio::process::Command;
    use windows_sys::Win32::System::Threading::PROCESS_TERMINATE;

    /// Exits 0 at once.
    pub(crate) const EXITS: &str = "process.exit(0)";

    /// Exits 0 after printing `terminated` when its stdin closes, the only stop
    /// request on Windows, and prints `ready` once it listens for that.
    pub(crate) const COOPERATIVE: &str = concat!(
        "process.stdin.on('end', () => { console.log('terminated'); process.exit(0); }); ",
        "process.stdin.resume(); console.log('ready');",
    );

    /// Ignores stdin closing, and prints `ready` once running.
    pub(crate) const STUBBORN: &str =
        "require('node:fs').writeSync(1, 'ready\\n'); setInterval(() => {}, 1e9);";

    /// Defines `serve(server, path)`, which serves `server` on the channel the
    /// child receives as `socket_path`. On Windows the core owns the pipe, so
    /// the child keeps four dials waiting and replaces each one the core takes,
    /// as `dialDesktopPipe` does.
    pub(crate) const SERVE: &str = concat!(
        "function serve(server, path) { ",
        "const waiting = new Set(); ",
        "const dial = () => { let connected = false; const socket = require('node:net').connect(path); ",
        "socket.once('error', () => { if (!connected) { socket.destroy(); setTimeout(dial, 50).unref(); } }); ",
        "socket.once('connect', () => { connected = true; waiting.add(socket); ",
        "socket.once('close', () => { if (waiting.delete(socket)) dial(); }); server.emit('connection', socket); }); }; ",
        "server.on('request', (request) => { if (waiting.delete(request.socket)) dial(); }); ",
        "for (let lane = 0; lane < 4; lane++) dial(); }\n",
    );

    /// How the supervisor records a child that terminated itself with
    /// `process.kill(process.pid, 'SIGTERM')`: libuv calls `TerminateProcess`
    /// with exit code 1, and Windows has no signals.
    pub(crate) fn self_terminated() -> serde_json::Value {
        serde_json::json!({"kind":"exit_code","value":1})
    }

    /// End the child the way an outside actor would: terminate its process.
    pub(crate) fn kill_child(pid: u32) {
        let process = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid) };
        assert!(!process.is_null());
        let terminated = unsafe { TerminateProcess(process, 1) };
        unsafe { CloseHandle(process) };
        assert_ne!(terminated, 0);
    }

    pub(crate) fn exited(code: u8) -> ExitStatus {
        ExitStatus::from_raw(u32::from(code))
    }

    /// `STATUS_STACK_BUFFER_OVERRUN`, the code `abort()` ends a process with.
    pub(crate) fn crashed() -> ExitStatus {
        ExitStatus::from_raw(0xC000_0409)
    }

    /// Ended by the core's job termination.
    pub(crate) fn killed() -> ExitStatus {
        ExitStatus::from_raw(JOB_KILL_EXIT_CODE)
    }

    /// The core forces no stop other than the job termination.
    pub(crate) fn terminated() -> ExitStatus {
        killed()
    }

    /// Create `command` inside its own kill-on-close job, adopted so the
    /// PID-based group helpers reach the job. The guard owns the job.
    pub(crate) fn spawn_group(command: &mut Command) -> (Child, Option<ParentDeathGuard>) {
        let lease = File::open("NUL").unwrap();
        let (setup, mut guard) = configure_child(command, &lease).unwrap();
        let child = spawn(command, &setup).unwrap();
        adopt(setup, &mut guard, &child).unwrap();
        (child, Some(guard))
    }

    /// The bound pipe needs no separate listener.
    pub(crate) struct Listener;

    pub(crate) fn channel(runtime_dir: &Path, generation: &str) -> (Channel, Listener) {
        (Channel::bind(runtime_dir, generation).unwrap(), Listener)
    }

    /// Open the channel's pipe as a client would. Once `close` has dropped
    /// every server instance the pipe name is gone, so this fails with
    /// `NotFound`; while the listener is up it succeeds at once.
    pub(crate) async fn channel_open(channel: &Channel) -> std::io::Result<()> {
        tokio::net::windows::named_pipe::ClientOptions::new()
            .open(channel.name())
            .map(drop)
    }

    /// Dial the channel's pipe and hold the connection unanswered.
    pub(crate) async fn hold_connection(_listener: Listener, name: &Path) -> File {
        let name = name.to_path_buf();
        tokio::task::spawn_blocking(move || OpenOptions::new().read(true).write(true).open(name))
            .await
            .unwrap()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows_sys::Win32::{
        Foundation::DuplicateHandle,
        Foundation::GENERIC_WRITE,
        System::{
            JobObjects::IsProcessInJob,
            Threading::{GetCurrentProcess, PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION},
        },
    };

    const JOB_OWNER_ROOT: &str = "PP_COMPAT_JOB_OWNER_ROOT";
    const SPAWN_EXIT_ROOT: &str = "PP_COMPAT_SPAWN_EXIT_ROOT";

    /// The core for the startup test: it exits the moment its child exists,
    /// before adoption or any other setup, and runs no cleanup.
    #[tokio::test]
    async fn spawn_exit_fixture() {
        let Some(root) = std::env::var_os(SPAWN_EXIT_ROOT) else {
            return;
        };
        let mut command = test_shell("setInterval(() => {}, 1e9)");
        let lease = File::open("NUL").unwrap();
        let (setup, _guard) = configure_child(&mut command, &lease).unwrap();
        let child = spawn(&mut command, &setup).unwrap();
        let parent: u32 = std::env::var("PP_COMPAT_SPAWN_EXIT_PARENT")
            .unwrap()
            .parse()
            .unwrap();
        let parent = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, parent) };
        assert!(!parent.is_null());
        let mut duplicated: HANDLE = std::ptr::null_mut();
        check(unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                child.raw(),
                parent,
                &mut duplicated,
                PROCESS_SYNCHRONIZE,
                0,
                0,
            )
        })
        .unwrap();
        std::fs::write(
            PathBuf::from(root).join("child"),
            (duplicated as usize).to_string(),
        )
        .unwrap();
        std::process::exit(0);
    }

    /// No startup gap (T-3): a core that exits right after creating its
    /// child, before anything else runs, still takes the child with it,
    /// because the child was created inside the kill-on-close job.
    #[test]
    fn ticket_t_3_drain_job_object_kills_child_when_core_exits_at_spawn() {
        let root = std::env::temp_dir().join(format!(
            "pp-spawn-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "windows::tests::spawn_exit_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SPAWN_EXIT_ROOT, &root)
            .env(
                "PP_COMPAT_SPAWN_EXIT_PARENT",
                std::process::id().to_string(),
            )
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        let handle: usize = std::fs::read_to_string(root.join("child"))
            .unwrap()
            .parse()
            .unwrap();
        // The fixture duplicated its child's handle into this process, so a
        // reused PID cannot stand in for the child.
        let process = unsafe { OwnedHandle::from_raw_handle(handle as _) };
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, 10_000) },
            WAIT_OBJECT_0,
            "child outlived the core that created it"
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

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

    /// A child that ignores its stdin closing past the grace period is
    /// terminated through its job, and the stop still counts as planned:
    /// the job termination and the leader's own termination both exit with
    /// `JOB_KILL_EXIT_CODE`, so whichever lands first, the code is the core's.
    #[tokio::test]
    async fn ticket_t_3_drain_forced_stop_after_grace_is_planned_shutdown() {
        let mut command = test_shell("setInterval(() => {}, 1e9)");
        let lease = File::open("NUL").unwrap();
        let (setup, mut guard) = configure_child(&mut command, &lease).unwrap();
        let mut child = spawn(&mut command, &setup).unwrap();
        adopt(setup, &mut guard, &child).unwrap();
        let pid = child.id().unwrap();
        drop(child.stdin.take());
        let report = crate::drain::drive_shutdown_group(
            &mut child,
            pid,
            true,
            Duration::from_millis(500),
            None,
        )
        .await
        .unwrap();
        assert!(report.grace_exhausted);
        assert!(report.group_absent);
        let crate::drain::GroupExit::Forced(status) = report.exit else {
            panic!("a child that ignores stdin must be forced");
        };
        assert_eq!(status.code(), Some(JOB_KILL_EXIT_CODE as i32));
        assert!(
            crate::validate_stop_outcome(
                crate::StopOutcome::Forced(status),
                crate::StopIntent::PlannedShutdown,
            )
            .is_ok()
        );
    }

    /// Dial the pipe the way the child does.
    fn dial_pipe(name: &Path) -> std::io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .access_mode(FILE_GENERIC_READ | READ_CONTROL | GENERIC_WRITE)
            .open(name)
    }

    async fn ping_pong(server: &mut Stream, client: &mut File) {
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

    #[tokio::test]
    async fn private_pipe_is_first_instance_owner_only_and_serves_each_connect() {
        let generation = format!("test-{}", rand::random::<u64>());
        let runtime = std::env::temp_dir();
        let channel = Channel::bind(&runtime, &generation).unwrap();
        assert!(Channel::bind(&runtime, &generation).is_err());
        for _ in 0..2 {
            let name = channel.name().to_path_buf();
            let client = tokio::task::spawn_blocking(move || dial_pipe(&name));
            let mut server = channel.connect().await.unwrap();
            let mut client = client.await.unwrap().unwrap();
            assert!(
                CurrentUser::query()
                    .unwrap()
                    .owns_privately(&client, SE_FILE_OBJECT)
                    .unwrap()
            );
            ping_pong(&mut server, &mut client).await;
        }
        channel.close().await.unwrap();
        assert!(channel.connect().await.is_err());
    }

    /// A dial that closes before the core takes it, as when Node reaps an idle
    /// socket, leaves the listening instance dead. The request that finds it
    /// replaces the instance and still connects to the child's redial.
    #[tokio::test]
    async fn private_pipe_connect_survives_a_dial_closed_before_it_was_taken() {
        let generation = format!("test-{}", rand::random::<u64>());
        let channel = Channel::bind(&std::env::temp_dir(), &generation).unwrap();
        let name = channel.name().to_path_buf();
        drop(dial_pipe(&name).unwrap());
        let redial = tokio::task::spawn_blocking(move || {
            let started = std::time::Instant::now();
            loop {
                match dial_pipe(&name) {
                    Ok(client) => return client,
                    Err(error) if started.elapsed() > CONNECT_TIMEOUT => panic!("{error}"),
                    Err(_) => std::thread::sleep(Duration::from_millis(20)),
                }
            }
        });
        let mut server = channel.connect().await.unwrap();
        let mut client = redial.await.unwrap();
        ping_pong(&mut server, &mut client).await;
    }
}
