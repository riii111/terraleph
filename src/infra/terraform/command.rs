use std::{
    env,
    ffi::{OsStr, OsString},
    fmt::{Display, Formatter},
    io::{self, Read},
    ops::Range,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use crate::app::execution::{
    EventStream, ExecutionEvent, ExecutionEventKind, ProcessExitStatus, ProcessTermination, Tool,
};
use crate::infra::CancellationToken;

use super::events::TerraformEventParser;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TerraformCommand {
    Init,
    Plan,
    Show,
    Apply,
    WorkspaceShow,
    StatePull,
    ProvidersSchema,
}

impl Display for TerraformCommand {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Init => "init",
            Self::Plan => "plan",
            Self::Show => "show",
            Self::Apply => "apply",
            Self::WorkspaceShow => "workspace show",
            Self::StatePull => "state pull",
            Self::ProvidersSchema => "providers schema",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessStatus {
    Exited(i32),
    Signaled,
}

impl ProcessStatus {
    #[must_use]
    pub(super) const fn is_success(self) -> bool {
        matches!(self, Self::Exited(0))
    }

    #[must_use]
    pub(crate) const fn is_plan_success(self) -> bool {
        matches!(self, Self::Exited(0 | 2))
    }

    /// `-detailed-exitcode` reports a plan with changes as exit status 2.
    #[must_use]
    pub(crate) const fn has_plan_changes(self) -> bool {
        matches!(self, Self::Exited(2))
    }

    #[must_use]
    pub(crate) const fn code(self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(code),
            Self::Signaled => None,
        }
    }
}

impl Display for ProcessStatus {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited(code) => write!(formatter, "exit status {code}"),
            Self::Signaled => formatter.write_str("terminated by signal"),
        }
    }
}

// No Debug: the output is raw Terraform text that can include sensitive values.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProcessOutput {
    pub(super) stdout: Vec<u8>,
    pub(super) stderr: Vec<u8>,
    ordered: Vec<ProcessOutputRange>,
}

#[derive(Clone, PartialEq, Eq)]
struct ProcessOutputRange {
    stream: EventStream,
    range: Range<usize>,
}

impl ProcessOutput {
    #[must_use]
    pub(super) const fn empty() -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            ordered: Vec::new(),
        }
    }

    fn append(&mut self, chunk: &ProcessOutputChunk) {
        let range = match chunk.stream {
            EventStream::Stdout => {
                let start = self.stdout.len();
                self.stdout.extend_from_slice(&chunk.bytes);
                start..self.stdout.len()
            }
            EventStream::Stderr => {
                let start = self.stderr.len();
                self.stderr.extend_from_slice(&chunk.bytes);
                start..self.stderr.len()
            }
        };
        self.ordered.push(ProcessOutputRange {
            stream: chunk.stream,
            range,
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TerraformExecutionErrorKind {
    Launch {
        command: TerraformCommand,
        message: String,
    },
    Process {
        command: TerraformCommand,
        message: String,
    },
    NonZero {
        command: TerraformCommand,
        status: ProcessStatus,
    },
    Interrupted {
        command: TerraformCommand,
    },
    InvalidOutput {
        command: TerraformCommand,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TerraformExecutionError {
    tool: Tool,
    kind: TerraformExecutionErrorKind,
}

impl TerraformExecutionError {
    pub(crate) const fn is_interrupted(&self) -> bool {
        matches!(
            self.kind,
            TerraformExecutionErrorKind::Interrupted { .. }
                | TerraformExecutionErrorKind::NonZero {
                    status: ProcessStatus::Signaled | ProcessStatus::Exited(130),
                    ..
                }
        )
    }

    const fn new_for_tool(tool: Tool, kind: TerraformExecutionErrorKind) -> Self {
        Self { tool, kind }
    }
}

impl Display for TerraformExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        let tool = self.tool.display_name();
        match &self.kind {
            TerraformExecutionErrorKind::Launch { command, message } => {
                write!(formatter, "failed to start {tool} {command}: {message}")
            }
            TerraformExecutionErrorKind::Process { command, message } => {
                write!(
                    formatter,
                    "failed while running {tool} {command}: {message}"
                )
            }
            TerraformExecutionErrorKind::NonZero { command, status } => {
                write!(formatter, "{tool} {command} failed with {status}")
            }
            TerraformExecutionErrorKind::Interrupted { command } => {
                write!(formatter, "{tool} {command} was interrupted")
            }
            TerraformExecutionErrorKind::InvalidOutput { command, message } => {
                write!(
                    formatter,
                    "{tool} {command} output could not be parsed: {message}"
                )
            }
        }
    }
}

impl std::error::Error for TerraformExecutionError {}

pub(super) struct ProcessResult {
    pub(super) status: ProcessStatus,
    pub(super) output: ProcessOutput,
    pub(super) interrupted: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ProcessOutputChunk {
    pub(super) stream: EventStream,
    pub(super) bytes: Vec<u8>,
}

pub(crate) trait ProcessRunner {
    fn start(
        &self,
        tool: Tool,
        root: &Path,
        arguments: &[OsString],
    ) -> io::Result<Box<dyn RunningProcess>>;
}

pub(crate) trait RunningProcess {
    fn poll_output(&mut self) -> io::Result<Vec<ProcessOutputChunk>> {
        Ok(Vec::new())
    }

    fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>>;
    fn request_interrupt(&mut self) -> io::Result<()>;
    fn wait(&mut self) -> io::Result<ProcessStatus>;
    fn collect_output(self: Box<Self>) -> io::Result<ProcessOutput>;
}

pub(crate) struct SystemProcessRunner;

pub(crate) fn resolve_executable(tool: Tool) -> io::Result<std::path::PathBuf> {
    let current = std::env::current_exe()?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(if cfg!(windows) {
            format!("{}.exe", tool.executable_name())
        } else {
            tool.executable_name().to_owned()
        });
        if !is_executable(&candidate) {
            continue;
        }
        let candidate = if candidate.is_absolute() {
            candidate
        } else {
            std::env::current_dir()?.join(candidate)
        };
        if same_executable(&candidate, &current)? {
            return Err(io::Error::other(format!(
                "{} resolves to Terraleph itself",
                tool.display_name()
            )));
        }
        return Ok(candidate);
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("{} was not found in PATH", tool.display_name()),
    ))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(windows)]
    {
        true
    }
}

fn same_executable(candidate: &Path, current: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let candidate = candidate.metadata()?;
        let current = current.metadata()?;
        Ok(candidate.dev() == current.dev() && candidate.ino() == current.ino())
    }
    #[cfg(windows)]
    {
        use std::{fs::File, os::windows::io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };

        fn identity(path: &Path) -> io::Result<(u32, u32, u32)> {
            let file = File::open(path)?;
            let mut information = BY_HANDLE_FILE_INFORMATION::default();
            // SAFETY: the handle stays open and information points to writable storage.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut information) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok((
                information.dwVolumeSerialNumber,
                information.nFileIndexHigh,
                information.nFileIndexLow,
            ))
        }
        Ok(identity(candidate)? == identity(current)?)
    }
}

pub(crate) fn delegate(
    executable: &Path,
    arguments: &[OsString],
) -> io::Result<std::process::ExitCode> {
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec())
    }
    #[cfg(windows)]
    {
        let mut process = SystemRunningProcess::new(command.spawn()?);
        let status = process.child.wait()?;
        drop(process);
        exit_delegated_process(status)
    }
}

pub(crate) fn run_passthrough(
    executable: &Path,
    root: &Path,
    arguments: &[OsString],
) -> io::Result<ProcessStatus> {
    let mut command = Command::new(executable);
    command
        .current_dir(root)
        .args(arguments)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    remove_cli_argument_environment(&mut command);
    #[cfg(unix)]
    {
        run_passthrough_unix(command)
    }
    #[cfg(windows)]
    {
        command.status().map(process_status)
    }
}

#[cfg(unix)]
fn run_passthrough_unix(mut command: Command) -> io::Result<ProcessStatus> {
    let previous = catch_uncaught_interrupt()?;
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            restore_interrupt(previous);
            return Err(error);
        }
    };
    let result = child.wait().map(process_status);
    restore_interrupt(previous);
    result
}

// Terraform receives the terminal's SIGINT itself, so Terraleph must survive it without
// forwarding. A handler the caller already installed does that and keeps the interrupt recorded;
// only the default or ignored disposition is replaced, the latter so the child still sees SIGINT.
#[cfg(unix)]
fn catch_uncaught_interrupt() -> io::Result<Option<libc::sighandler_t>> {
    const extern "C" fn ignore_interrupt(_: libc::c_int) {}

    // SAFETY: a zeroed sigaction is a valid output buffer for querying the current disposition.
    let mut current: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action only reads the current disposition into `current`.
    if unsafe { libc::sigaction(libc::SIGINT, std::ptr::null(), &raw mut current) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if current.sa_sigaction != libc::SIG_DFL && current.sa_sigaction != libc::SIG_IGN {
        return Ok(None);
    }
    // SAFETY: the handler performs no work and is restored after the child exits.
    let previous = unsafe {
        libc::signal(
            libc::SIGINT,
            ignore_interrupt as *const () as libc::sighandler_t,
        )
    };
    if previous == libc::SIG_ERR {
        return Err(io::Error::last_os_error());
    }
    Ok(Some(previous))
}

#[cfg(unix)]
fn restore_interrupt(previous: Option<libc::sighandler_t>) {
    if let Some(previous) = previous {
        // SAFETY: restores the disposition captured before the child was spawned.
        unsafe { libc::signal(libc::SIGINT, previous) };
    }
}

fn remove_cli_argument_environment(command: &mut Command) {
    for (name, _) in
        env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("TF_CLI_ARGS"))
    {
        command.env_remove(name);
    }
}

#[cfg(windows)]
#[expect(
    clippy::exit,
    reason = "stable ExitCode only accepts u8; Windows delegation must preserve all 32 exit-status bits after reaping the child"
)]
fn exit_delegated_process(status: ExitStatus) -> ! {
    std::process::exit(status.code().unwrap_or(1));
}

pub(super) fn run_command(
    tool: Tool,
    root: &Path,
    command: TerraformCommand,
    arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    mut event_sink: Option<&mut dyn FnMut(ExecutionEvent)>,
) -> Result<ProcessResult, TerraformExecutionError> {
    let mut parser = event_sink.is_some().then(TerraformEventParser::new);
    let mut observed_chunks = 0;
    if cancellation.is_cancelled() {
        if let Some(event_sink) = event_sink {
            emit_termination(event_sink, ProcessStatus::Signaled, true);
        }
        return Ok(ProcessResult {
            status: ProcessStatus::Signaled,
            output: ProcessOutput::empty(),
            interrupted: true,
        });
    }

    let mut process = runner.start(tool, root, arguments).map_err(|error| {
        TerraformExecutionError::new_for_tool(
            tool,
            TerraformExecutionErrorKind::Launch {
                command,
                message: error.to_string(),
            },
        )
    })?;
    let process_error = |error: io::Error| {
        TerraformExecutionError::new_for_tool(
            tool,
            TerraformExecutionErrorKind::Process {
                command,
                message: error.to_string(),
            },
        )
    };

    loop {
        let chunks = process.poll_output().map_err(process_error)?;
        if let (Some(parser), Some(event_sink)) = (parser.as_mut(), event_sink.as_deref_mut()) {
            emit_chunks(parser, &mut observed_chunks, chunks, event_sink);
        }
        let (status, interrupted) = match process.try_wait().map_err(process_error)? {
            Some(status) => (status, false),
            None if cancellation.is_cancelled() => {
                let _ = process.request_interrupt();
                (process.wait().map_err(process_error)?, true)
            }
            None => {
                thread::sleep(PROCESS_POLL_INTERVAL);
                continue;
            }
        };
        let output = process.collect_output().map_err(process_error)?;
        if let (Some(parser), Some(event_sink)) = (parser.as_mut(), event_sink.as_deref_mut()) {
            emit_unobserved_output(parser, observed_chunks, &output, event_sink);
            emit_parser_remainders(parser, event_sink);
            emit_termination(event_sink, status, interrupted);
        }
        return Ok(ProcessResult {
            status,
            output,
            interrupted,
        });
    }
}

pub(super) fn run_successful(
    tool: Tool,
    root: &Path,
    command: TerraformCommand,
    arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    event_sink: Option<&mut dyn FnMut(ExecutionEvent)>,
) -> Result<ProcessOutput, TerraformExecutionError> {
    let process = run_command(
        tool,
        root,
        command,
        arguments,
        cancellation,
        runner,
        event_sink,
    )?;
    if process.interrupted {
        return Err(interrupted_error(tool, command));
    }
    if !process.status.is_success() {
        return Err(non_zero_error(tool, command, process.status));
    }
    Ok(process.output)
}

fn emit_chunks(
    parser: &mut TerraformEventParser,
    observed_chunks: &mut usize,
    chunks: Vec<ProcessOutputChunk>,
    event_sink: &mut dyn FnMut(ExecutionEvent),
) {
    for chunk in chunks {
        *observed_chunks += 1;
        for event in parser.push(chunk.stream, &chunk.bytes, Instant::now()) {
            event_sink(event);
        }
    }
}

fn emit_unobserved_output(
    parser: &mut TerraformEventParser,
    observed_chunks: usize,
    output: &ProcessOutput,
    event_sink: &mut dyn FnMut(ExecutionEvent),
) {
    for record in output.ordered.iter().skip(observed_chunks) {
        let bytes = match record.stream {
            EventStream::Stdout => &output.stdout[record.range.clone()],
            EventStream::Stderr => &output.stderr[record.range.clone()],
        };
        for event in parser.push(record.stream, bytes, Instant::now()) {
            event_sink(event);
        }
    }
}

fn emit_parser_remainders(
    parser: &mut TerraformEventParser,
    event_sink: &mut dyn FnMut(ExecutionEvent),
) {
    for stream in [EventStream::Stdout, EventStream::Stderr] {
        for event in parser.finish(stream, Instant::now()) {
            event_sink(event);
        }
    }
}

fn emit_termination(
    event_sink: &mut dyn FnMut(ExecutionEvent),
    status: ProcessStatus,
    interrupted: bool,
) {
    let status = match status {
        ProcessStatus::Exited(code) => ProcessExitStatus::Exited(code),
        ProcessStatus::Signaled => ProcessExitStatus::Signaled,
    };
    event_sink(ExecutionEvent {
        received_at: Instant::now(),
        kind: ExecutionEventKind::Terminated(ProcessTermination {
            status,
            interrupted,
        }),
    });
}

pub(super) const fn interrupted_error(
    tool: Tool,
    command: TerraformCommand,
) -> TerraformExecutionError {
    TerraformExecutionError::new_for_tool(
        tool,
        TerraformExecutionErrorKind::Interrupted { command },
    )
}

pub(super) const fn non_zero_error(
    tool: Tool,
    command: TerraformCommand,
    status: ProcessStatus,
) -> TerraformExecutionError {
    TerraformExecutionError::new_for_tool(
        tool,
        TerraformExecutionErrorKind::NonZero { command, status },
    )
}

pub(super) fn invalid_output(
    tool: Tool,
    command: TerraformCommand,
    message: impl Display,
) -> TerraformExecutionError {
    TerraformExecutionError::new_for_tool(
        tool,
        TerraformExecutionErrorKind::InvalidOutput {
            command,
            message: message.to_string(),
        },
    )
}

impl ProcessRunner for SystemProcessRunner {
    fn start(
        &self,
        tool: Tool,
        root: &Path,
        arguments: &[OsString],
    ) -> io::Result<Box<dyn RunningProcess>> {
        let mut command = Command::new(OsStr::new(tool.executable_name()));
        command
            .current_dir(root)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        remove_cli_argument_environment(&mut command);
        configure_process_group(&mut command);
        let child = command.spawn()?;
        Ok(Box::new(SystemRunningProcess::new(child)))
    }
}

struct SystemRunningProcess {
    child: Child,
    chunks: Receiver<io::Result<ProcessOutputChunk>>,
    readers: Vec<JoinHandle<io::Result<()>>>,
    output: ProcessOutput,
}

impl SystemRunningProcess {
    fn new(mut child: Child) -> Self {
        let (sender, chunks) = mpsc::channel();
        let mut readers = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            readers.push(spawn_reader(stdout, EventStream::Stdout, sender.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            readers.push(spawn_reader(stderr, EventStream::Stderr, sender));
        }
        Self {
            child,
            chunks,
            readers,
            output: ProcessOutput::empty(),
        }
    }
}

impl RunningProcess for SystemRunningProcess {
    fn poll_output(&mut self) -> io::Result<Vec<ProcessOutputChunk>> {
        drain_output_chunks(&self.chunks, &mut self.output)
    }

    fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>> {
        self.child
            .try_wait()
            .map(|status| status.map(process_status))
    }

    fn request_interrupt(&mut self) -> io::Result<()> {
        request_interrupt(&self.child)
    }

    fn wait(&mut self) -> io::Result<ProcessStatus> {
        self.child.wait().map(process_status)
    }

    fn collect_output(mut self: Box<Self>) -> io::Result<ProcessOutput> {
        let reader_result = join_readers(&mut self.readers);
        let chunk_result = drain_output_chunks(&self.chunks, &mut self.output);
        let output = std::mem::replace(&mut self.output, ProcessOutput::empty());
        reader_result.and(chunk_result).map(|_| output)
    }
}

impl Drop for SystemRunningProcess {
    fn drop(&mut self) {
        let running = self.child.try_wait().ok().flatten().is_none();
        if running {
            let _ = request_interrupt(&self.child);
            let _ = self.child.wait();
        }
        let _ = join_readers(&mut self.readers);
    }
}

#[cfg(unix)]
const fn configure_process_group(_command: &mut Command) {}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn request_interrupt(child: &Child) -> io::Result<()> {
    let pid = i32::try_from(child.id()).map_err(|_| io::Error::other("child PID is too large"))?;
    // SAFETY: kill is called with the live child PID and a valid signal constant.
    let result = unsafe { libc::kill(pid, libc::SIGINT) };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn request_interrupt(child: &Child) -> io::Result<()> {
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};

    // SAFETY: the child was created as its own process group and its PID is that group ID.
    let result = unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, child.id()) };
    if result != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn process_status(status: ExitStatus) -> ProcessStatus {
    if status.success() {
        ProcessStatus::Exited(0)
    } else {
        status
            .code()
            .map_or(ProcessStatus::Signaled, ProcessStatus::Exited)
    }
}

fn spawn_reader<R>(
    mut reader: R,
    stream: EventStream,
    sender: Sender<io::Result<ProcessOutputChunk>>,
) -> JoinHandle<io::Result<()>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut buffer = [0_u8; 8 * 1024];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(count) => count,
                Err(error) => {
                    let _ = sender.send(Err(io::Error::new(error.kind(), error.to_string())));
                    return Err(error);
                }
            };
            if count == 0 {
                return Ok(());
            }
            sender
                .send(Ok(ProcessOutputChunk {
                    stream,
                    bytes: buffer[..count].to_vec(),
                }))
                .map_err(|_| io::Error::other("Terraform output receiver was dropped"))?;
        }
    })
}

fn drain_output_chunks(
    receiver: &Receiver<io::Result<ProcessOutputChunk>>,
    output: &mut ProcessOutput,
) -> io::Result<Vec<ProcessOutputChunk>> {
    let mut chunks = Vec::new();
    loop {
        match receiver.try_recv() {
            Ok(Ok(chunk)) => {
                output.append(&chunk);
                chunks.push(chunk);
            }
            Ok(Err(error)) => return Err(error),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(chunks),
        }
    }
}

fn join_readers(readers: &mut Vec<JoinHandle<io::Result<()>>>) -> io::Result<()> {
    let mut first_error = None;
    for reader in readers.drain(..) {
        match reader.join() {
            Ok(Err(error)) if first_error.is_none() => first_error = Some(error),
            Err(_) if first_error.is_none() => {
                first_error = Some(io::Error::other("Terraform output reader panicked"));
            }
            Ok(Ok(()) | Err(_)) | Err(_) => {}
        }
    }
    first_error.map_or(Ok(()), Err)
}

// Shared by sibling Terraform tests because the represented fields stay private
// to this implementation module in production.
#[cfg(test)]
mod tests {
    use crate::app::execution::{Diagnostic, DiagnosticSource};

    use super::*;

    impl ProcessOutput {
        pub(crate) fn new(stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
            let mut output = Self::empty();
            for (stream, bytes) in [(EventStream::Stdout, stdout), (EventStream::Stderr, stderr)] {
                if !bytes.is_empty() {
                    output.append(&ProcessOutputChunk { stream, bytes });
                }
            }
            output
        }

        pub(crate) fn from_chunks<'a>(
            chunks: impl IntoIterator<Item = &'a ProcessOutputChunk>,
        ) -> Self {
            let mut output = Self::empty();
            for chunk in chunks {
                output.append(chunk);
            }
            output
        }

        #[must_use]
        pub(crate) fn stdout(&self) -> &[u8] {
            &self.stdout
        }
    }

    impl TerraformExecutionError {
        pub(crate) const fn kind(&self) -> &TerraformExecutionErrorKind {
            &self.kind
        }
    }

    fn non_json_text(event: &ExecutionEvent) -> Option<(EventStream, &str)> {
        let ExecutionEventKind::Diagnostic(Diagnostic {
            summary,
            source: DiagnosticSource::NonJson { stream },
            ..
        }) = &event.kind
        else {
            return None;
        };
        Some((*stream, summary.as_str()))
    }

    #[test]
    fn unobserved_output_replays_remaining_ranges_in_receive_order() {
        let message = "初期化\n".as_bytes();
        let split = "初".len() - 1;
        let mut chunks = vec![
            ProcessOutputChunk {
                stream: EventStream::Stdout,
                bytes: message[..split].to_vec(),
            },
            ProcessOutputChunk {
                stream: EventStream::Stderr,
                bytes: b"warning\n".to_vec(),
            },
            ProcessOutputChunk {
                stream: EventStream::Stdout,
                bytes: message[split..].to_vec(),
            },
            ProcessOutputChunk {
                stream: EventStream::Stdout,
                bytes: b"final line".to_vec(),
            },
        ];
        let mut output = ProcessOutput::empty();
        for chunk in &chunks {
            output.append(chunk);
        }

        let mut parser = TerraformEventParser::new();
        let mut observed = 0;
        let mut events = Vec::new();
        emit_chunks(
            &mut parser,
            &mut observed,
            vec![chunks.remove(0)],
            &mut |event| events.push(event),
        );

        emit_unobserved_output(&mut parser, observed, &output, &mut |event| {
            events.push(event);
        });
        emit_parser_remainders(&mut parser, &mut |event| events.push(event));

        assert_eq!(output.stdout(), [message, b"final line"].concat());
        assert_eq!(output.stderr, b"warning\n");
        assert_eq!(
            events.iter().map(non_json_text).collect::<Vec<_>>(),
            [
                Some((EventStream::Stderr, "warning")),
                Some((EventStream::Stdout, "初期化")),
                Some((EventStream::Stdout, "final line")),
            ]
        );
    }
}
