// SPDX-License-Identifier: GPL-3.0-or-later

//! Standard Windows and Xbox Scarlett Host Services Implementation for Xenora X15.
//!
//! Provides verifiable guest implementations of Windows NT and Xbox Scarlett APIs:
//! - Time: UTC file time and a monotonic performance counter
//! - Process and thread identity and guest process priority boost policy
//! - Error slots: `GetLastError`, `SetLastError`
//! - Virtual memory: `VirtualAlloc`, `VirtualFree`
//! - Xbox physical memory: `XMemAlloc`, `XMemFree`

mod codepages;
mod compare;
mod console;
mod crt;
mod crt_arguments;
mod crt_callbacks;
mod crt_clock;
mod crt_files;
mod crt_format;
mod crt_math;
mod crt_startup;
mod crt_streams;
mod crt_wformat;
mod crt_wide;
mod cxx;
mod environment;
mod errors;
mod events;
mod file_mapping;
mod files;
mod gdk_queue;
mod graphics;
mod heap;
mod locale;
mod modules;
mod platform_fixture;
mod process;
mod registry;
mod startup_info;
mod static_cxx;
mod synch;
pub mod sysinfo;
mod task_fixture;
pub(crate) mod threads;
mod winsock;

use crate::runtime::HResult;
pub use crt_arguments::{MAX_PROGRAM_ARGUMENT_BYTES, ProgramArgumentsError};
pub use crt_streams::MAX_CAPTURED_OUTPUT;
pub use startup_info::{ProcessStartupInfo, StartupInfoError};

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{ALLOCATION_GRANULARITY, GuestMemory, MemoryError, MemoryPermissions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

const CURRENT_PROCESS: u64 = u64::MAX;
const CURRENT_THREAD: u64 = u64::MAX - 1;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_ADDRESS: u32 = 487;
const THREAD_QUERY_RIGHTS: u32 = 0x0040 | 0x0800;
const THREAD_SET_RIGHTS: u32 = 0x0020 | 0x0400;
const THREAD_SUPPORTED_RIGHTS: u32 = THREAD_QUERY_RIGHTS | THREAD_SET_RIGHTS | 0x0010_0000;
const THREAD_PRIORITY_ERROR_RETURN: u64 = 0x7fff_ffff;
const MAX_OBJECT_HANDLES: usize = 1024;
const DUPLICATE_SAME_ACCESS: u32 = 2;
const HANDLE_API: &str = "api-ms-win-core-handle-l1-1-0.dll";
const PAGE_SIZE: u64 = 4096;
const MEM_COMMIT_RESERVE: u32 = 0x3000;
const MEM_RELEASE: u32 = 0x8000;
const ERROR_NOACCESS: u32 = 998;
const KERNEL32: &str = "kernel32.dll";
const SYSINFO_API: &str = "api-ms-win-core-sysinfo-l1-1-0.dll";
const PROCESS_THREADS_V1: &str = "api-ms-win-core-processthreads-l1-1-0.dll";

const PERFORMANCE_FREQUENCY: u64 = 10_000_000;
const MAX_PERFORMANCE_COUNT: u64 = i64::MAX as u64;

/// Clock trait for injecting deterministic time in tests or host time in production.
pub trait Clock: Send + Sync {
    /// Return Windows 100-nanosecond intervals since January 1, 1601 (UTC).
    fn now_filetime(&self) -> u64;

    /// Return elapsed monotonic time in 100-nanosecond ticks, capped at `i64::MAX`.
    fn performance_count(&self) -> u64 {
        static ORIGIN: OnceLock<Instant> = OnceLock::new();
        let ticks = ORIGIN.get_or_init(Instant::now).elapsed().as_nanos() / 100;
        ticks.min(u128::from(MAX_PERFORMANCE_COUNT)) as u64
    }

    /// Suspend the calling guest thread for `intervals` 100-nanosecond ticks.
    fn sleep(&self, intervals: u64) {
        std::thread::sleep(std::time::Duration::from_nanos(
            intervals.saturating_mul(100),
        ));
    }
}

/// Deterministic test clock.
pub struct MockClock {
    current: AtomicU64,
    performance_count: AtomicU64,
}

impl MockClock {
    pub fn new(initial_filetime: u64) -> Self {
        Self {
            current: AtomicU64::new(initial_filetime),
            performance_count: AtomicU64::new(0),
        }
    }

    /// Move both readings forward by `intervals` 100-nanosecond ticks. Each
    /// stops at its maximum, so neither ever runs backwards.
    pub fn advance(&self, intervals: u64) {
        self.current
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                Some(current.saturating_add(intervals))
            })
            .expect("clock update always returns a value");
        self.performance_count
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                Some(count.saturating_add(intervals).min(MAX_PERFORMANCE_COUNT))
            })
            .expect("clock update always returns a value");
    }
}

impl Clock for MockClock {
    fn now_filetime(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
    }

    fn performance_count(&self) -> u64 {
        self.performance_count.load(Ordering::SeqCst)
    }

    /// Sleeping advances the mock clock instead of waiting.
    fn sleep(&self, intervals: u64) {
        self.advance(intervals);
    }
}

/// Host system UTC clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_filetime(&self) -> u64 {
        // Windows FILETIME epoch is 1601-01-01 00:00:00 UTC.
        // Unix epoch is 1970-01-01 00:00:00 UTC (difference = 11,644,473,600 seconds).
        const WINDOWS_TICK: u64 = 10_000_000;
        const SEC_TO_UNIX_EPOCH: u64 = 11_644_473_600;
        let now = std::time::SystemTime::now();
        let dur = now
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let unix_secs = dur.as_secs();
        let nanos = dur.subsec_nanos() as u64;
        let windows_secs = unix_secs + SEC_TO_UNIX_EPOCH;
        windows_secs * WINDOWS_TICK + (nanos / 100)
    }
}

enum GuestObject {
    Thread(u32),
    File(files::OpenFile),
    Search(files::Search),
    Waitable {
        state: Arc<Mutex<events::Waitable>>,
        access: u32,
    },
    FileMapping {
        size: u64,
    },
}

#[derive(Default)]
struct ObjectState {
    priority: i32,
    last_handle: u64,
    handles: BTreeMap<u64, GuestObject>,
    files: BTreeMap<String, Vec<u8>>,
    directories: BTreeSet<String>,
    next_temp: u32,
}

impl ObjectState {
    fn next_handle(&self) -> Option<u64> {
        self.last_handle
            .checked_add(4)
            .filter(|_| self.handles.len() < MAX_OBJECT_HANDLES)
    }

    fn insert_handle(&mut self, handle: u64, object: GuestObject) {
        self.handles.insert(handle, object);
        self.last_handle = handle;
    }

    fn check_access(&self, handle: u64, required: u32) -> Result<(), u32> {
        if handle == CURRENT_THREAD {
            return Ok(());
        }
        match self.handles.get(&handle) {
            Some(GuestObject::Thread(access)) if access & required != 0 => Ok(()),
            Some(GuestObject::Thread(_)) => Err(ERROR_ACCESS_DENIED),
            _ => Err(ERROR_INVALID_HANDLE),
        }
    }
}

/// State container for runtime services.
pub struct ServicesState {
    pub clock: Arc<dyn Clock>,
    clock_origin: u64,
    pub process_id: u32,
    priority_boost_disabled: AtomicBool,
    pub thread_id: u32,
    /// Characteristics the guest is told it runs on. These are simulated and
    /// independent of the host; see [`sysinfo::HostCapabilities`] for the host.
    pub guest_profile: sysinfo::GuestSystemProfile,
    objects: Mutex<ObjectState>,
    last_errors: Mutex<BTreeMap<u64, u32>>,
    error_mode: Mutex<u32>,
    crt_arguments: Mutex<crt_arguments::CrtArguments>,
    console: Mutex<ConsoleState>,
    startup: Mutex<startup_info::StartupState>,
    synch: Mutex<synch::Synch>,
    crt_streams: Mutex<crt_streams::StandardStreams>,
    crt_clock: Mutex<crt_clock::ClockServices>,
    pub(crate) threads: Mutex<threads::ThreadTable>,
    pub(crate) hooks: Arc<threads::SchedulerHooks>,
    /// Functions registered with `atexit`, run in reverse order by `_cexit`.
    atexit: Mutex<Vec<u64>>,
    /// Emulated thread-local storage: the block each guest thread owns for
    /// each thread-local variable, keyed by the guest thread and then by the
    /// address of the variable's descriptor.
    pub(crate) emutls: Mutex<BTreeMap<u64, BTreeMap<u64, u64>>>,
    /// The status the guest process exited with. Set once.
    exit_status: Mutex<Option<u32>>,
    crt_startup: Mutex<crt_startup::CrtStartup>,
    winsock: Mutex<winsock::Winsock>,
    task_fixture: Mutex<task_fixture::TaskFixture>,
    platform_fixture: Mutex<platform_fixture::PlatformFixture>,
    gdk_queues: Mutex<gdk_queue::QueueHandles>,
    gdk_queue_changed: Condvar,
    winsock_changed: Condvar,
    pub next_alloc_addr: AtomicU64,
    pub graphics: Arc<Mutex<crate::graphics::GraphicsContext>>,
}

/// The console state: the mode bits round-tripping per handle and the
/// redirected standard handles, package 8 first part of #459. The defaults
/// are the fixed pseudo-handles of `services/console.rs`.
#[derive(Default)]
struct ConsoleState {
    input_handle: Option<u64>,
    output_handle: Option<u64>,
    error_handle: Option<u64>,
    input_mode: u32,
    output_mode: u32,
    initialized: bool,
}

impl ConsoleState {
    fn handle(&mut self, kind: u32) -> Option<u64> {
        self.initialize();
        match kind {
            console::STD_INPUT => Some(self.input_handle.unwrap()),
            console::STD_OUTPUT => Some(self.output_handle.unwrap()),
            console::STD_ERROR => Some(self.error_handle.unwrap()),
            _ => None,
        }
    }

    fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        self.input_handle = Some(console::INPUT_HANDLE);
        self.output_handle = Some(console::OUTPUT_HANDLE);
        self.error_handle = Some(console::ERROR_HANDLE);
        self.input_mode = console::DEFAULT_INPUT_MODE;
        self.output_mode = console::DEFAULT_OUTPUT_MODE;
        self.initialized = true;
    }
}

impl ServicesState {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock_origin: clock.performance_count(),
            clock,
            priority_boost_disabled: AtomicBool::new(false),
            process_id: 0x4100, // Synthetic process ID
            thread_id: 0x4104,  // Synthetic main thread ID
            guest_profile: sysinfo::GuestSystemProfile::default(),
            objects: Mutex::new(ObjectState::default()),
            last_errors: Mutex::default(),
            error_mode: Mutex::new(0),
            crt_arguments: Mutex::default(),
            startup: Mutex::default(),
            synch: Mutex::default(),
            crt_streams: Mutex::default(),
            crt_clock: Mutex::default(),
            console: Mutex::default(),
            threads: Mutex::default(),
            hooks: Arc::default(),
            atexit: Mutex::default(),
            emutls: Mutex::default(),
            exit_status: Mutex::default(),
            crt_startup: Mutex::default(),
            winsock: Mutex::default(),
            task_fixture: Mutex::default(),
            platform_fixture: Mutex::default(),
            gdk_queues: Mutex::default(),
            gdk_queue_changed: Condvar::new(),
            winsock_changed: Condvar::new(),
            next_alloc_addr: AtomicU64::new(0x0000_0002_0000_0000), // 8GB guest heap
            graphics: Arc::new(Mutex::new(crate::graphics::GraphicsContext::new())),
        }
    }
    /// Give a hosted C program the arguments its `main` receives in `argv`,
    /// program name first. Call it before running the guest: the C runtime's
    /// startup code builds `argv` once, and a call after that, or after the
    /// guest has read its command line, is refused.
    ///
    /// The strings reach the guest as their bytes; no code page is applied.
    /// A host that never calls this gives the program one argument, the empty
    /// string. An empty slice gives it none.
    /// Give a hosted C program the bytes it reads from `stdin`, before the
    /// run. Text mode is applied when they are stored: CR LF pairs become
    /// one line feed. Calling it after the guest started has no effect on
    /// what it already read, so hosts should call it before running.
    pub fn set_standard_input(&self, bytes: &[u8]) {
        self.crt_streams().set_input(bytes);
    }

    pub fn set_program_arguments(&self, arguments: &[&str]) -> Result<(), ProgramArgumentsError> {
        self.crt_arguments().supply(arguments)
    }

    fn crt_arguments(&self) -> std::sync::MutexGuard<'_, crt_arguments::CrtArguments> {
        self.crt_arguments.lock().expect("program arguments")
    }

    /// Exchange the process error mode, returning the previous one. The
    /// mode controls error dialogs that do not exist here; it is stored so
    /// programs that propagate it - a parent passing it to children, say -
    /// behave coherently.
    pub(crate) fn exchange_error_mode(&self, mode: u32) -> u32 {
        let mut guard = self.error_mode.lock().expect("error mode");
        let previous = *guard;
        *guard = mode;
        previous
    }

    fn uptime_millis(&self) -> u64 {
        let ticks = self
            .clock
            .performance_count()
            .saturating_sub(self.clock_origin);
        ticks / (PERFORMANCE_FREQUENCY / 1_000)
    }

    /// The scheduling policy recorded for the synthetic process.
    pub fn priority_boost_disabled(&self) -> bool {
        self.priority_boost_disabled.load(Ordering::SeqCst)
    }

    /// The status the guest process exited with, once it has exited.
    pub fn exit_status(&self) -> Option<u32> {
        *self.exit_status.lock().expect("guest exit status")
    }

    /// How many guest handles are open: kernel objects, sockets and task
    /// queues, which are kept in three separate tables.
    pub fn open_handle_count(&self) -> usize {
        let objects = self
            .objects
            .lock()
            .expect("guest object state")
            .handles
            .len();
        let sockets = self
            .winsock
            .lock()
            .expect("guest Winsock state")
            .open_count();
        let queues = self
            .gdk_queues
            .lock()
            .expect("GDK queue handle table")
            .open_count();
        objects + sockets + queues
    }

    /// A file on the synthetic `C:` drive, named as the guest names it.
    pub fn file_contents(&self, path: &str) -> Option<Vec<u8>> {
        let path = path.replace('/', "\\").to_ascii_lowercase();
        let name = path.strip_prefix("c:\\").unwrap_or(&path);
        self.objects
            .lock()
            .expect("guest object state")
            .files
            .get(name)
            .cloned()
    }

    /// End the guest process. The first exit decides the status; a later one
    /// reports it again and changes nothing.
    ///
    /// Every guest handle is closed: kernel objects, sockets and task queues.
    /// Files stay on the synthetic drive, guest memory stays mapped for the
    /// host to inspect, and `atexit` functions are not run.
    fn exit_process(&self, status: u32) -> u32 {
        let mut exited = self.exit_status.lock().expect("guest exit status");
        if let Some(first) = *exited {
            return first;
        }
        *exited = Some(status);
        // The three table locks are taken one at a time, never nested.
        self.objects
            .lock()
            .expect("guest object state")
            .handles
            .clear();
        self.winsock
            .lock()
            .expect("guest Winsock state")
            .close_all();
        self.winsock_changed.notify_all();
        self.gdk_queues
            .lock()
            .expect("GDK queue handle table")
            .close_all();
        self.gdk_queue_changed.notify_all();
        status
    }

    /// Everything the guest has written to `stdout`, as the bytes a text-mode
    /// stream hands the operating system: each line feed the program wrote is
    /// a carriage return and a line feed here.
    pub fn standard_output(&self) -> Vec<u8> {
        self.crt_streams().output().to_vec()
    }

    /// Everything the guest has written to `stderr`, in the same form as
    /// [`Self::standard_output`].
    pub fn standard_error(&self) -> Vec<u8> {
        self.crt_streams().error().to_vec()
    }

    fn crt_streams(&self) -> std::sync::MutexGuard<'_, crt_streams::StandardStreams> {
        self.crt_streams.lock().expect("standard streams")
    }

    fn console(&self) -> std::sync::MutexGuard<'_, ConsoleState> {
        self.console.lock().expect("console state")
    }

    /// Read the last-error value for a guest thread's TEB.
    pub fn last_error(&self, teb: u64) -> u32 {
        self.last_errors
            .lock()
            .expect("guest last-error state")
            .get(&teb)
            .copied()
            .unwrap_or(0)
    }

    /// Forget the last-error value of a guest thread that has ended, so a
    /// later thread given the same TEB address starts at zero. Returns
    /// whether a nonzero value was dropped.
    ///
    /// Each service call names its own TEB and the values sit behind one
    /// lock, so calls for different threads may run concurrently. Retire a
    /// TEB only once no service call for that thread is still running.
    pub fn retire_thread_last_error(&self, teb: u64) -> bool {
        self.last_errors
            .lock()
            .expect("guest last-error state")
            .remove(&teb)
            .is_some()
    }

    fn set_last_error(&self, teb: u64, error: u32) {
        let mut errors = self.last_errors.lock().expect("guest last-error state");
        if error == 0 {
            errors.remove(&teb);
        } else {
            errors.insert(teb, error);
        }
    }

    fn crt_startup(&self) -> std::sync::MutexGuard<'_, crt_startup::CrtStartup> {
        self.crt_startup.lock().expect("C runtime startup state")
    }

    /// The application type the guest passed to `_set_app_type`, zero until it
    /// does. Nothing in the runtime depends on it.
    pub fn crt_app_type(&self) -> i32 {
        self.crt_startup().app_type()
    }

    /// The guest function passed to `__setusermatherr`, zero until one is. It
    /// is never called.
    pub fn crt_user_math_error(&self) -> u64 {
        self.crt_startup().user_math_error()
    }

    /// Record a guest error and return the documented failure value.
    fn fail<T>(&self, teb: u64, error: u32, value: T) -> Result<T, ExitReason> {
        self.set_last_error(teb, error);
        Ok(value)
    }

    fn is_current_process(&self, teb: u64, handle: u64) -> bool {
        if handle == CURRENT_PROCESS {
            return true;
        }
        self.set_last_error(teb, ERROR_INVALID_HANDLE);
        false
    }
}

/// Extract the memory error of a variable-cell operation. The cell providers
/// only fail through memory faults, so any other exit reason is unreachable;
/// it maps to `Overflow` rather than panic because a loader must not crash on
/// guest state corruption.
fn cell_memory(result: Result<u64, ExitReason>) -> Result<u64, MemoryError> {
    result.map_err(|error| match error {
        ExitReason::MemoryFault(memory) => memory,
        _ => MemoryError::Overflow,
    })
}

/// Register the services generated from `runtime/services.txt` and the
/// hand-written fixture services.
pub fn register_standard_services(registry: &mut ImportRegistry, state: Arc<ServicesState>) {
    crate::runtime_table::register(registry, &state);
    registry.hooks = Some(Arc::clone(&state.hooks));
    threads::register(registry, &state);
    static_cxx::register(registry);
    crate::exceptions::register(registry);

    // The legacy CRT variables a msvcrt.dll-linked image imports as data
    // (#643). Each cell is the one the modern accessors maintain, so the
    // legacy data view and the function view cannot disagree. `__initenv`
    // shares the `_environ` cell: the environment never changes here, which
    // keeps the two identical, as they start on Windows. `_timezone` stays
    // zero with the documented UTC clock.
    for (symbol, variable) in [
        ("__argc", crt_arguments::Variable::Argc),
        ("__argv", crt_arguments::Variable::Argv),
        ("_environ", crt_arguments::Variable::Environ),
        ("__initenv", crt_arguments::Variable::Environ),
    ] {
        let s = state.clone();
        registry.register_data(
            "msvcrt.dll",
            symbol,
            ServiceStatus::Partial,
            move |memory| cell_memory(s.crt_arguments().address(memory, variable)),
        );
    }
    for (symbol, index) in [("_fmode", 0_u64), ("_commode", 1_u64)] {
        let s = state.clone();
        registry.register_data(
            "msvcrt.dll",
            symbol,
            ServiceStatus::Partial,
            move |memory| cell_memory(s.crt_startup().file_mode_address(memory, index)),
        );
    }
    {
        let s = state.clone();
        registry.register_data(
            "msvcrt.dll",
            "_timezone",
            ServiceStatus::Partial,
            move |memory| cell_memory(s.crt_startup().timezone_address(memory)),
        );
    }

    let s = state.clone();
    registry.register(KERNEL32, "GetThreadId", move |args, cpu, mem| {
        let handle = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
        if handle == CURRENT_THREAD {
            return Ok(u64::from(s.thread_id_of(cpu.gs_base)));
        }
        let thread = s.objects.lock().expect("guest object state");
        if let Some(GuestObject::Waitable { state, access }) = thread.handles.get(&handle)
            && let events::Waitable::Thread { id, .. } =
                &*state.lock().expect("guest waitable state")
        {
            return if access & THREAD_QUERY_RIGHTS == 0 {
                s.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0)
            } else {
                Ok(u64::from(*id))
            };
        }
        if let Err(error) = thread.check_access(handle, THREAD_QUERY_RIGHTS) {
            return s.fail(cpu.gs_base, error, 0);
        }
        Ok(u64::from(s.thread_id))
    });

    registry.alias(PROCESS_THREADS_V1, KERNEL32, "GetThreadId");
    registry.set_status(KERNEL32, "GetThreadId", ServiceStatus::Partial);

    let s = state.clone();
    registry.register(KERNEL32, "DuplicateHandle", move |args, cpu, mem| {
        let source_process = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
        let source = args.get(1, mem).map_err(ExitReason::MemoryFault)?;
        let target_process = args.get(2, mem).map_err(ExitReason::MemoryFault)?;
        let output = args.get(3, mem).map_err(ExitReason::MemoryFault)?;
        let desired = args.get(4, mem).map_err(ExitReason::MemoryFault)? as u32;
        let inherit = args.get(5, mem).map_err(ExitReason::MemoryFault)? as u32;
        let options = args.get(6, mem).map_err(ExitReason::MemoryFault)? as u32;

        if source_process != CURRENT_PROCESS || target_process != CURRENT_PROCESS {
            return s.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0);
        }
        if inherit != 0 || options & !DUPLICATE_SAME_ACCESS != 0 || source == CURRENT_PROCESS {
            return s.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
        }
        let worker = s.current_worker(cpu.gs_base);
        let mut thread = s.objects.lock().expect("guest object state");
        let (granted, mut object) = if let (CURRENT_THREAD, Some(state)) = (source, worker) {
            let access = threads::THREAD_ALL_ACCESS;
            (access, GuestObject::Waitable { state, access })
        } else if source == CURRENT_THREAD {
            (
                THREAD_SUPPORTED_RIGHTS,
                GuestObject::Thread(THREAD_SUPPORTED_RIGHTS),
            )
        } else {
            match thread.handles.get(&source) {
                Some(GuestObject::Thread(rights)) => (*rights, GuestObject::Thread(*rights)),
                Some(GuestObject::Waitable { state, access }) => (
                    *access,
                    GuestObject::Waitable {
                        state: Arc::clone(state),
                        access: *access,
                    },
                ),
                Some(GuestObject::File(file)) => (file.access, GuestObject::File(file.clone())),
                Some(GuestObject::Search(_) | GuestObject::FileMapping { .. }) => {
                    return s.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
                }
                None => return s.fail(cpu.gs_base, ERROR_INVALID_HANDLE, 0),
            }
        };
        let access = if options & DUPLICATE_SAME_ACCESS != 0 {
            granted
        } else if desired & !granted == 0 {
            desired
        } else {
            return s.fail(cpu.gs_base, ERROR_ACCESS_DENIED, 0);
        };
        let Some(handle) = thread.next_handle() else {
            return s.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        if output == 0 || mem.write_u64(output, handle).is_err() {
            return s.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        match &mut object {
            GuestObject::Thread(rights) | GuestObject::Waitable { access: rights, .. } => {
                *rights = access
            }
            GuestObject::File(file) => file.access = access,
            GuestObject::Search(_) | GuestObject::FileMapping { .. } => {
                unreachable!("these handles cannot be duplicated")
            }
        }
        thread.insert_handle(handle, object);
        Ok(1)
    });
    registry.alias(HANDLE_API, KERNEL32, "DuplicateHandle");
    registry.set_status(KERNEL32, "DuplicateHandle", ServiceStatus::Partial);

    events::register(registry, &state);
    synch::register(registry, &state);
    sysinfo::register(registry, &state);
    files::register(registry, &state);
    graphics::register(registry, &state);
    crt::register(registry);
    crt::register_heap(registry);
    crt::register_errno(registry);
    crt::register_conversions(registry);
    crt::register_utilities(registry);
    crt_callbacks::register(registry, &state);
    environment::register(registry, &state);
    errors::register(registry, &state);
    registry::register(registry, &state);
    file_mapping::register(registry, &state);
    heap::register(registry, &state);
    cxx::register(registry, &state);

    let s = state.clone();
    registry.register(
        KERNEL32,
        "QueryPerformanceCounter",
        move |args, cpu, mem| {
            let ptr = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            if ptr == 0 || mem.write_u64(ptr, s.clock.performance_count()).is_err() {
                return s.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            Ok(1)
        },
    );

    let s = state.clone();
    registry.register(
        KERNEL32,
        "QueryPerformanceFrequency",
        move |args, cpu, mem| {
            let ptr = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
            if ptr == 0 || mem.write_u64(ptr, PERFORMANCE_FREQUENCY).is_err() {
                return s.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            Ok(1)
        },
    );

    for symbol in ["QueryPerformanceCounter", "QueryPerformanceFrequency"] {
        registry.alias("api-ms-win-core-profile-l1-1-0.dll", KERNEL32, symbol);
    }

    let s = state.clone();
    registry.register(KERNEL32, "GetTickCount", move |_, _, _| {
        Ok(u64::from(s.uptime_millis() as u32))
    });

    let s = state.clone();
    registry.register(KERNEL32, "GetTickCount64", move |_, _, _| {
        Ok(s.uptime_millis())
    });

    for symbol in ["GetTickCount", "GetTickCount64"] {
        registry.alias(SYSINFO_API, KERNEL32, symbol);
    }

    // 8. XMemAlloc(dwSize, dwAllocAttributes) -> LPVOID (Xbox Scarlett allocator in xmem.dll)
    let s = state.clone();
    let handler = move |args: &crate::imports::Win64Args,
                        _cpu: &mut crate::cpu::CpuState,
                        mem: &mut GuestMemory|
          -> Result<u64, ExitReason> {
        let size = args.get(0, mem).map_err(ExitReason::MemoryFault)? as usize;
        let _attr = args.get(1, mem).map_err(ExitReason::MemoryFault)?;

        if size == 0 {
            return Ok(0);
        }

        let page_size = 4096usize;
        // Huge or overflowing sizes fail closed. Unbounded map_region would
        // try to malloc multi-gigabyte host buffers (libFuzzer OOM on
        // ~3.7 GiB); route through the virtual budget instead.
        let Some(aligned_size) = size
            .checked_add(page_size - 1)
            .map(|rounded| rounded & !(page_size - 1))
            .filter(|&aligned| aligned != 0)
        else {
            return Ok(0);
        };
        let alloc_addr = s
            .next_alloc_addr
            .fetch_add(aligned_size as u64, Ordering::SeqCst);

        Ok(mem
            .map_virtual_allocation(alloc_addr, aligned_size, MemoryPermissions::READ_WRITE)
            .unwrap_or(0))
    };
    registry.register("xmem.dll", "XMemAlloc", handler);
    registry.set_status("xmem.dll", "XMemAlloc", ServiceStatus::Partial);

    // 9. XMemFree(pAddress, dwAllocAttributes) -> VOID (Xbox Scarlett in xmem.dll)
    let handler = move |args: &crate::imports::Win64Args,
                        _cpu: &mut crate::cpu::CpuState,
                        mem: &mut GuestMemory|
          -> Result<u64, ExitReason> {
        let _addr = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
        Ok(0)
    };
    registry.register("xmem.dll", "XMemFree", handler);
    registry.set_status("xmem.dll", "XMemFree", ServiceStatus::Stub);

    // 10. Synthetic Test Graphics Device Creation (xenora_fixture.dll)
    // Prototype graphics device creation for original integration fixtures.
    // In guest ABI: ppDevice is Arg 3 (R9). Writes generation-checked device handle.
    let s_gfx = state.graphics.clone();
    let handler = move |args: &crate::imports::Win64Args,
                        _cpu: &mut crate::cpu::CpuState,
                        mem: &mut GuestMemory|
          -> Result<u64, ExitReason> {
        let pp_device = args.get(3, mem).map_err(ExitReason::MemoryFault)?;
        if pp_device == 0 {
            return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
        }
        let mut gfx = s_gfx
            .lock()
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        match gfx.create_device() {
            Ok(handle) => {
                mem.write_u64(pp_device, handle.to_u64())
                    .map_err(ExitReason::MemoryFault)?;
                Ok(HResult::OK.guest_return())
            }
            Err(_) => Ok(HResult::FAIL.guest_return()),
        }
    };
    registry.register("xenora_fixture.dll", "CreateGraphicsDevice", handler);
    registry.set_status(
        "xenora_fixture.dll",
        "CreateGraphicsDevice",
        ServiceStatus::Implemented,
    );

    // 11. Synthetic Test Command Submission & Presentation (xenora_fixture.dll)
    // Prototype graphics submission service:
    // Arg 0 (RCX): Guest address of vertex buffer (12 x f32 coordinates)
    // Arg 1 (RDX): Clear color packed as RGBA u32 or pointer to 4 floats
    // Arg 2 (R8): Viewport dimensions (width in lower 32 bits, height in upper 32 bits)
    // Arg 3 (R9): Optional guest output pointer for readback status or frame index
    let s_gfx = state.graphics.clone();
    let handler = move |args: &crate::imports::Win64Args,
                        _cpu: &mut crate::cpu::CpuState,
                        mem: &mut GuestMemory|
          -> Result<u64, ExitReason> {
        let vtx_addr = args.get(0, mem).map_err(ExitReason::MemoryFault)?;
        if vtx_addr == 0 {
            return Ok(HResult::from_win32(ERROR_INVALID_PARAMETER).guest_return());
        }
        let clear_raw = args.get(1, mem).map_err(ExitReason::MemoryFault)? as u32;
        let clear_color = [
            (clear_raw & 0xFF) as u8,
            ((clear_raw >> 8) & 0xFF) as u8,
            ((clear_raw >> 16) & 0xFF) as u8,
            ((clear_raw >> 24) & 0xFF) as u8,
        ];
        let dims_raw = args.get(2, mem).map_err(ExitReason::MemoryFault)?;
        let width = (dims_raw as u32) as usize;
        let height = ((dims_raw >> 32) as u32) as usize;
        let (vp_w, vp_h) = if width > 0 && height > 0 {
            (width, height)
        } else {
            (4, 3)
        };

        let mut gfx = s_gfx
            .lock()
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;

        // Build and execute guest command list through GraphicsContext
        let dev = gfx
            .create_device()
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        let queue = gfx
            .create_command_queue(dev, crate::graphics::CommandListType::Direct)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        let alloc = gfx
            .create_command_allocator(dev, crate::graphics::CommandListType::Direct)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        let list = gfx
            .create_command_list(dev, alloc, crate::graphics::CommandListType::Direct)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;

        let vb_desc = crate::graphics::ResourceDesc {
            dimension: crate::graphics::ResourceDimension::Buffer,
            width: 48,
            height: 1,
            depth_or_array_size: 1,
            mip_levels: 1,
            format_rgba8: false,
        };
        let vb = gfx
            .create_committed_resource(
                dev,
                vb_desc,
                crate::graphics::ResourceStates::VERTEX_AND_CONSTANT_BUFFER,
                Some(vtx_addr),
            )
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;

        gfx.cmd_set_viewport(list, vp_w, vp_h)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        gfx.cmd_set_render_targets(list, None, clear_color)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        gfx.cmd_set_vertex_buffer(list, vb, 0, 48)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        gfx.cmd_draw_instanced(list, 3, 1, 0, 0)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        gfx.close_command_list(list)
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;

        let outcome = gfx.execute_command_list(queue, list, mem).map_err(|e| {
            ExitReason::IllegalInstruction(match e {
                crate::graphics::GraphicsError::NativeMetalRequired => 0xFE,
                _ => 0xFF,
            })
        })?;

        let pp_out = args.get(3, mem).map_err(ExitReason::MemoryFault)?;
        if pp_out != 0 {
            mem.write_u64(pp_out, outcome.pixels.len() as u64)
                .map_err(ExitReason::MemoryFault)?;
        }

        Ok(HResult::OK.guest_return())
    };
    registry.register("xenora_fixture.dll", "SubmitGuestDraw", handler);
    registry.set_status(
        "xenora_fixture.dll",
        "SubmitGuestDraw",
        ServiceStatus::Implemented,
    );
    winsock::register(registry, &state);
    task_fixture::register(registry, &state);
    platform_fixture::register(registry, &state);
    gdk_queue::register(registry, &state);
    startup_info::register(registry, &state);
    process::register(registry, &state);
    // Last: the module catalog lists every service registered before it.
    modules::register(registry, &state);
}

/// Implementations behind the generated adapters. A failure the guest can
/// observe sets its last error; `Err` stops the guest.
pub(crate) mod handlers {
    use super::*;
    use crate::runtime::{Float32, Float64, OutPtr, ServiceContext};
    use crate::softfloat::{Control, F32, F64, square_root};

    type Outcome<T> = Result<T, ExitReason>;

    /// The exit for a request the runtime does not model.
    const RUNTIME_LIMIT: ExitReason = ExitReason::IllegalInstruction(0xFE);

    pub fn set_process_priority_boost(
        context: &mut ServiceContext<'_>,
        process: u64,
        disable: bool,
    ) -> Outcome<bool> {
        if !context.state.is_current_process(context.teb, process) {
            return Ok(false);
        }
        context
            .state
            .priority_boost_disabled
            .store(disable, Ordering::SeqCst);
        Ok(true)
    }

    pub fn get_process_priority_boost(
        context: &mut ServiceContext<'_>,
        process: u64,
        disabled: OutPtr<4>,
    ) -> Outcome<bool> {
        if !context.state.is_current_process(context.teb, process) {
            return Ok(false);
        }
        let value = u32::from(context.state.priority_boost_disabled());
        if disabled.store(context.memory, value.to_le_bytes()).is_err() {
            return context.state.fail(context.teb, ERROR_NOACCESS, false);
        }
        Ok(true)
    }

    pub fn get_current_process(_: &mut ServiceContext<'_>) -> Outcome<u64> {
        Ok(CURRENT_PROCESS)
    }

    pub fn get_process_id(context: &mut ServiceContext<'_>, process: u64) -> Outcome<u32> {
        Ok(if context.state.is_current_process(context.teb, process) {
            context.state.process_id
        } else {
            0
        })
    }

    pub fn get_current_thread(_: &mut ServiceContext<'_>) -> Outcome<u64> {
        Ok(CURRENT_THREAD)
    }

    pub fn get_current_process_id(context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(context.state.process_id)
    }

    pub fn get_current_thread_id(context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(context.state.thread_id_of(context.teb))
    }

    pub fn open_thread(
        context: &mut ServiceContext<'_>,
        access: u32,
        inherit: bool,
        id: u32,
    ) -> Outcome<u64> {
        let state = context.state;
        let worker = state.worker_object(id);
        let main = id == state.thread_id && state.main_thread_alive();
        if !main && worker.is_none() {
            return state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        let supported = THREAD_SUPPORTED_RIGHTS
            | if worker.is_some() {
                threads::WORKER_RIGHTS
            } else {
                0
            };
        if inherit || access & !supported != 0 {
            return state.fail(context.teb, ERROR_NOT_SUPPORTED, 0);
        }
        let mut thread = state.objects.lock().expect("guest object state");
        let Some(handle) = thread.next_handle() else {
            return state.fail(context.teb, ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        let object = match worker {
            Some(state) => GuestObject::Waitable { state, access },
            None => GuestObject::Thread(access),
        };
        thread.insert_handle(handle, object);
        Ok(handle)
    }

    pub fn get_thread_priority(context: &mut ServiceContext<'_>, handle: u64) -> Outcome<i32> {
        // Only the main thread has a priority; a worker must not change it.
        if handle == CURRENT_THREAD && context.state.current_worker(context.teb).is_some() {
            return context.state.fail(
                context.teb,
                ERROR_NOT_SUPPORTED,
                THREAD_PRIORITY_ERROR_RETURN as i32,
            );
        }
        let thread = context.state.objects.lock().expect("guest object state");
        if let Err(error) = thread.check_access(handle, THREAD_QUERY_RIGHTS) {
            return context
                .state
                .fail(context.teb, error, THREAD_PRIORITY_ERROR_RETURN as i32);
        }
        Ok(thread.priority)
    }

    pub fn set_thread_priority(
        context: &mut ServiceContext<'_>,
        handle: u64,
        priority: i32,
    ) -> Outcome<bool> {
        // Only the main thread has a priority; a worker must not change it.
        if handle == CURRENT_THREAD && context.state.current_worker(context.teb).is_some() {
            return context.state.fail(context.teb, ERROR_NOT_SUPPORTED, false);
        }
        let state = context.state;
        let mut thread = state.objects.lock().expect("guest object state");
        if let Err(error) = thread.check_access(handle, THREAD_SET_RIGHTS) {
            return state.fail(context.teb, error, false);
        }
        if matches!(priority, 0x0001_0000 | 0x0002_0000) {
            return state.fail(context.teb, ERROR_NOT_SUPPORTED, false);
        }
        if !matches!(priority, -15 | -2..=2 | 15) {
            return state.fail(context.teb, ERROR_INVALID_PARAMETER, false);
        }
        thread.priority = priority;
        Ok(true)
    }

    pub fn close_handle(context: &mut ServiceContext<'_>, handle: u64) -> Outcome<bool> {
        let mut objects = context.state.objects.lock().expect("guest object state");
        if matches!(objects.handles.get(&handle), Some(GuestObject::Search(_))) {
            return context.state.fail(context.teb, ERROR_INVALID_HANDLE, false);
        }
        let closed = matches!(handle, CURRENT_PROCESS | CURRENT_THREAD)
            || objects.handles.remove(&handle).is_some();
        if closed {
            Ok(true)
        } else {
            context.state.fail(context.teb, ERROR_INVALID_HANDLE, false)
        }
    }

    pub fn get_system_time_as_file_time(
        context: &mut ServiceContext<'_>,
        time: OutPtr<8>,
    ) -> Outcome<()> {
        let now = context.state.clock.now_filetime();
        time.store(context.memory, now.to_le_bytes())
            .map_err(ExitReason::MemoryFault)
    }

    // Streams, files and formatted output (#400, #402). `crt_streams.rs` and
    // `crt_files.rs` hold the rules; a failure that has an errno value sets it
    // here. The stream state is always locked before the object state.

    fn or_errno<T>(
        context: &mut ServiceContext<'_>,
        outcome: crt_streams::Outcome<T>,
    ) -> Outcome<T> {
        match outcome? {
            Ok(value) => Ok(value),
            Err(crt_streams::Failed(value, errno)) => {
                crt::set_errno(context.memory, context.teb, errno)?;
                Ok(value)
            }
        }
    }

    pub fn standard_stream(context: &mut ServiceContext<'_>, index: u32) -> Outcome<u64> {
        context.state.crt_streams().file(context.memory, index)
    }

    pub fn vfprintf(
        context: &mut ServiceContext<'_>,
        options: u64,
        stream: u64,
        format: u64,
        locale: u64,
        arguments: u64,
    ) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.print(
                context.memory,
                &mut drive,
                options,
                stream,
                format,
                locale,
                arguments,
            )
        };
        or_errno(context, outcome)
    }

    pub fn fwrite(
        context: &mut ServiceContext<'_>,
        buffer: u64,
        size: u64,
        count: u64,
        stream: u64,
    ) -> Outcome<u64> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.write_items(context.memory, &mut drive, buffer, size, count, stream)
        };
        or_errno(context, outcome)
    }

    // The buffer forms of printf, and the secure forms (#466). They share the
    // formatter through `crt_streams`; the stream state only serves `putchar`
    // and `vfprintf_s`.

    #[allow(clippy::too_many_arguments)]
    pub fn vsprintf(
        context: &mut ServiceContext<'_>,
        options: u64,
        buffer: u64,
        count: u64,
        format: u64,
        locale: u64,
        arguments: u64,
    ) -> Outcome<i32> {
        let outcome = crt_streams::print_to_buffer(
            context.memory,
            options,
            buffer,
            count,
            format,
            locale,
            arguments,
        );
        or_errno(context, outcome)
    }

    pub fn vswprintf(
        context: &mut ServiceContext<'_>,
        options: u64,
        buffer: u64,
        count: u64,
        format: u64,
        locale: u64,
        arguments: u64,
    ) -> Outcome<i32> {
        let outcome = crt_streams::print_to_wide_buffer(
            context.memory,
            options,
            buffer,
            count,
            format,
            locale,
            arguments,
        );
        or_errno(context, outcome)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn vsprintf_s(
        context: &mut ServiceContext<'_>,
        options: u64,
        buffer: u64,
        count: u64,
        format: u64,
        locale: u64,
        arguments: u64,
    ) -> Outcome<i32> {
        let outcome = crt_streams::print_to_buffer_secure(
            context.memory,
            options,
            buffer,
            count,
            format,
            locale,
            arguments,
        );
        or_errno(context, outcome)
    }

    pub fn vfprintf_s(
        context: &mut ServiceContext<'_>,
        options: u64,
        stream: u64,
        format: u64,
        locale: u64,
        arguments: u64,
    ) -> Outcome<i32> {
        // The secure stream form validates what the plain one validates, and
        // `%n` is invalid in both, so they are the same here.
        vfprintf(context, options, stream, format, locale, arguments)
    }

    pub fn putchar(context: &mut ServiceContext<'_>, character: i32) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.put_character_stdout(&mut drive, character)
        };
        or_errno(context, outcome)
    }

    pub fn fputs(context: &mut ServiceContext<'_>, text: u64, stream: u64) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.put_string(context.memory, &mut drive, text, stream)
        };
        or_errno(context, outcome)
    }

    /// `puts`: the string and a newline to stdout. Returns a nonnegative
    /// value, or `EOF` with `errno` set.
    pub fn puts(context: &mut ServiceContext<'_>, text: u64) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.put_line_stdout(context.memory, &mut drive, text)
        };
        or_errno(context, outcome)
    }

    pub fn getchar(context: &mut ServiceContext<'_>) -> Outcome<i32> {
        Ok(context.state.crt_streams().character_stdin())
    }

    pub fn feof(context: &mut ServiceContext<'_>, stream: u64) -> Outcome<i32> {
        let outcome = context.state.crt_streams().end_of_file(stream);
        or_errno(context, outcome)
    }

    pub fn fflush(context: &mut ServiceContext<'_>, stream: u64) -> Outcome<i32> {
        context.state.crt_streams().flush(stream)
    }

    pub fn setvbuf(
        context: &mut ServiceContext<'_>,
        stream: u64,
        _buffer: u64,
        mode: i32,
        size: u64,
    ) -> Outcome<i32> {
        let outcome = context.state.crt_streams().set_buffer(stream, mode, size);
        or_errno(context, outcome)
    }

    pub fn fopen(context: &mut ServiceContext<'_>, name: u64, mode: u64) -> Outcome<u64> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.open(context.memory, &mut drive, name, mode)
        };
        or_errno(context, outcome)
    }

    pub fn fclose(context: &mut ServiceContext<'_>, stream: u64) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.close(context.memory, &mut drive, stream)
        };
        or_errno(context, outcome)
    }

    pub fn tmpfile(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.open_temporary(context.memory, &mut drive)
        };
        or_errno(context, outcome)
    }

    pub fn tmpnam(context: &mut ServiceContext<'_>, buffer: u64) -> Outcome<u64> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.temporary_name(context.memory, &drive, buffer)
        };
        or_errno(context, outcome)
    }

    pub fn fgets(
        context: &mut ServiceContext<'_>,
        buffer: u64,
        count: i32,
        stream: u64,
    ) -> Outcome<u64> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.get_line(context.memory, &drive, buffer, count, stream)
        };
        or_errno(context, outcome)
    }

    pub fn fputws(context: &mut ServiceContext<'_>, text: u64, stream: u64) -> Outcome<u32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.write_wide_string(context.memory, &mut drive, text, stream)
        };
        or_errno(context, outcome)
    }

    pub fn fgetws(
        context: &mut ServiceContext<'_>,
        buffer: u64,
        count: i32,
        stream: u64,
    ) -> Outcome<u64> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.read_wide_line(
                context.memory,
                &mut drive,
                buffer,
                count as u32 as u64,
                stream,
            )
        };
        or_errno(context, outcome)
    }

    pub fn putwchar(context: &mut ServiceContext<'_>, unit: u32) -> Outcome<u32> {
        let outcome = context.state.crt_streams().wide_character_stdout(unit);
        or_errno(context, outcome)
    }

    pub fn fputwc(context: &mut ServiceContext<'_>, unit: u32, stream: u64) -> Outcome<u32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.write_wide_unit(&mut drive, unit, stream)
        };
        or_errno(context, outcome)
    }

    pub fn getwchar(context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(context.state.crt_streams().wide_character_stdin())
    }

    pub fn _ftelli64(context: &mut ServiceContext<'_>, stream: u64) -> Outcome<u64> {
        let outcome = context.state.crt_streams().tell(stream);
        or_errno(context, outcome).map(|position| position as u64)
    }

    pub fn _fseeki64(
        context: &mut ServiceContext<'_>,
        stream: u64,
        offset: u64,
        origin: i32,
    ) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.seek(&drive, stream, offset as i64, origin as u32)
        };
        or_errno(context, outcome)
    }

    pub fn fgetpos(context: &mut ServiceContext<'_>, stream: u64, position: u64) -> Outcome<i32> {
        let outcome = context
            .state
            .crt_streams()
            .get_position(context.memory, stream, position);
        or_errno(context, outcome)
    }

    pub fn fsetpos(context: &mut ServiceContext<'_>, stream: u64, position: u64) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.set_position(context.memory, &drive, stream, position)
        };
        or_errno(context, outcome)
    }

    pub fn remove(context: &mut ServiceContext<'_>, name: u64) -> Outcome<i32> {
        let outcome = {
            let streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.remove_file(context.memory, &mut drive, name)
        };
        or_errno(context, outcome)
    }

    pub fn rename(context: &mut ServiceContext<'_>, from: u64, to: u64) -> Outcome<i32> {
        let outcome = {
            let streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.rename_file(context.memory, &mut drive, from, to)
        };
        or_errno(context, outcome)
    }

    pub fn _open(
        context: &mut ServiceContext<'_>,
        name: u64,
        flags: i32,
        mode: i32,
    ) -> Outcome<i32> {
        let _ = mode;
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.open_descriptor(context.memory, &mut drive, name, flags as u32)
        };
        or_errno(context, outcome)
    }

    pub fn _close(context: &mut ServiceContext<'_>, fd: i32) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.close_descriptor(context.memory, &mut drive, fd)
        };
        or_errno(context, outcome)
    }

    pub fn _read(
        context: &mut ServiceContext<'_>,
        fd: i32,
        buffer: u64,
        count: u32,
    ) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.read_descriptor(context.memory, &drive, fd, buffer, count)
        };
        or_errno(context, outcome)
    }

    pub fn _write(
        context: &mut ServiceContext<'_>,
        fd: i32,
        buffer: u64,
        count: u32,
    ) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.write_descriptor(context.memory, &mut drive, fd, buffer, count)
        };
        or_errno(context, outcome)
    }

    pub fn _lseek(
        context: &mut ServiceContext<'_>,
        fd: i32,
        offset: i32,
        origin: i32,
    ) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.seek_descriptor(&drive, fd, offset, origin)
        };
        or_errno(context, outcome)
    }

    pub fn _fileno(context: &mut ServiceContext<'_>, stream: u64) -> Outcome<i32> {
        let outcome = context.state.crt_streams().descriptor_of(stream);
        or_errno(context, outcome)
    }

    pub fn _isatty(context: &mut ServiceContext<'_>, fd: i32) -> Outcome<i32> {
        let outcome = context.state.crt_streams().is_terminal(fd);
        or_errno(context, outcome)
    }

    pub fn _access(context: &mut ServiceContext<'_>, name: u64, mode: i32) -> Outcome<i32> {
        let outcome = {
            let streams = context.state.crt_streams();
            let drive = context.state.objects.lock().expect("guest object state");
            streams.access(context.memory, &drive, name, mode)
        };
        or_errno(context, outcome)
    }

    pub fn _unlink(context: &mut ServiceContext<'_>, name: u64) -> Outcome<i32> {
        // `_unlink` is `remove` by another name, as Microsoft's page says.
        remove(context, name)
    }

    pub fn fopen_s(
        context: &mut ServiceContext<'_>,
        file: u64,
        name: u64,
        mode: u64,
    ) -> Outcome<i32> {
        let outcome = {
            let mut streams = context.state.crt_streams();
            let mut drive = context.state.objects.lock().expect("guest object state");
            streams.open_secure(context.memory, &mut drive, file, name, mode)
        };
        or_errno(context, outcome)
    }

    pub fn clearerr(context: &mut ServiceContext<'_>, stream: u64) -> Outcome<()> {
        if stream == 0 {
            // The invalid parameter handler is not modelled; `errno` is
            // EINVAL, as the Microsoft page for `clearerr` says.
            crt::set_errno(context.memory, context.teb, crt::EINVAL)?;
        }
        context.state.crt_streams().clear_errors(stream)
    }

    pub fn perror(context: &mut ServiceContext<'_>, message: u64) -> Outcome<()> {
        let address = context
            .memory
            .crt_errno_address(context.teb)
            .map_err(ExitReason::MemoryFault)?;
        let errno = context
            .memory
            .read_u32(address)
            .map_err(ExitReason::MemoryFault)?;
        let mut streams = context.state.crt_streams();
        let mut drive = context.state.objects.lock().expect("guest object state");
        streams.report(context.memory, &mut drive, message, errno)
    }

    pub fn get_last_error(context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(context.state.last_error(context.teb))
    }

    pub fn set_last_error(context: &mut ServiceContext<'_>, code: u32) -> Outcome<()> {
        context.state.set_last_error(context.teb, code);
        Ok(())
    }

    // The encoding conversions: `services/codepages.rs` says which pages
    // Xenora serves and how each converts.

    fn wide_input(
        memory: &crate::memory::GuestMemory,
        address: u64,
        count: i32,
    ) -> Result<Vec<u16>, MemoryError> {
        if count == 0 || address == 0 {
            return Err(MemoryError::Overflow);
        }
        if count > 0 {
            let mut units = Vec::with_capacity(count as usize);
            for index in 0..count as u64 {
                let at = address
                    .checked_add(index * 2)
                    .ok_or(MemoryError::Overflow)?;
                units.push(memory.read_u16(at)?);
            }
            return Ok(units);
        }
        // -1: up to and including the terminating zero.
        let mut units = Vec::new();
        for index in 0..(u32::MAX as u64 + 1) {
            let at = address
                .checked_add(index * 2)
                .ok_or(MemoryError::Overflow)?;
            let unit = memory.read_u16(at)?;
            units.push(unit);
            if unit == 0 {
                return Ok(units);
            }
        }
        Err(MemoryError::Overflow)
    }

    pub fn multibyte_to_wide_char(
        context: &mut ServiceContext<'_>,
        codepage: u32,
        flags: u32,
        input: u64,
        input_count: i32,
        output: u64,
        output_count: i32,
    ) -> Outcome<i32> {
        use crate::services::codepages as pages;
        let state = context.state;
        let teb = context.teb;
        let memory = &mut *context.memory;
        let Some(page) = pages::resolve(codepage) else {
            return state.fail(teb, pages::ERROR_INVALID_PARAMETER, 0);
        };
        // MB_ERR_INVALID_CHARS is the only flag these pages take.
        if flags & !0x8 != 0 {
            return state.fail(teb, pages::ERROR_INVALID_FLAGS, 0);
        }
        if input_count == 0 || input_count < -1 || input == 0 {
            return state.fail(teb, pages::ERROR_INVALID_PARAMETER, 0);
        }
        let bytes = if input_count > 0 {
            let mut bytes = vec![0u8; input_count as usize];
            memory
                .read_bytes(input, &mut bytes)
                .map_err(ExitReason::MemoryFault)?;
            bytes
        } else {
            // -1: up to and including the terminating zero.
            let mut bytes = Vec::new();
            for offset in 0..(u32::MAX as u64 + 1) {
                let at = input
                    .checked_add(offset)
                    .ok_or(MemoryError::Overflow)
                    .map_err(ExitReason::MemoryFault)?;
                let mut one = [0u8];
                memory
                    .read_bytes(at, &mut one)
                    .map_err(ExitReason::MemoryFault)?;
                bytes.push(one[0]);
                if one[0] == 0 {
                    break;
                }
            }
            bytes
        };
        let (units, malformed) = pages::decode(page, &bytes);
        if !malformed.is_empty() && flags & 0x8 != 0 {
            return state.fail(teb, pages::ERROR_NO_UNICODE_TRANSLATION, 0);
        }
        if output_count == 0 {
            return Ok(units.len() as i32);
        }
        if output == 0 || (output_count as usize) < units.len() {
            return state.fail(teb, pages::ERROR_INSUFFICIENT_BUFFER, 0);
        }
        for (index, &unit) in units.iter().enumerate() {
            memory
                .write_u16(output + index as u64 * 2, unit)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(units.len() as i32)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn wide_char_to_multibyte(
        context: &mut ServiceContext<'_>,
        codepage: u32,
        flags: u32,
        input: u64,
        input_count: i32,
        output: u64,
        output_count: i32,
        default_char: u64,
        used_default: u64,
    ) -> Outcome<i32> {
        use crate::services::codepages as pages;
        let state = context.state;
        let teb = context.teb;
        let memory = &mut *context.memory;
        let Some(page) = pages::resolve(codepage) else {
            return state.fail(teb, pages::ERROR_INVALID_PARAMETER, 0);
        };
        // WC_ERR_INVALID_CHARS is the only flag these pages take. For UTF-8
        // the default-character arguments must both be null, as Microsoft's
        // page requires.
        if flags & !0x80 != 0 {
            return state.fail(teb, pages::ERROR_INVALID_FLAGS, 0);
        }
        if page == pages::CodePage::Utf8 && (default_char != 0 || used_default != 0) {
            return state.fail(teb, pages::ERROR_INVALID_PARAMETER, 0);
        }
        if input_count == 0 || input_count < -1 || input == 0 {
            return state.fail(teb, pages::ERROR_INVALID_PARAMETER, 0);
        }
        let units = wide_input(memory, input, input_count).map_err(ExitReason::MemoryFault)?;
        let default = if default_char != 0 {
            let mut one = [0u8];
            memory
                .read_bytes(default_char, &mut one)
                .map_err(ExitReason::MemoryFault)?;
            one[0]
        } else {
            b'?'
        };
        let (bytes, unrepresentable) = pages::encode(page, &units, default);
        if !unrepresentable.is_empty() && flags & 0x80 != 0 {
            return state.fail(teb, pages::ERROR_NO_UNICODE_TRANSLATION, 0);
        }
        if output_count == 0 {
            return Ok(bytes.len() as i32);
        }
        if output == 0 || (output_count as usize) < bytes.len() {
            return state.fail(teb, pages::ERROR_INSUFFICIENT_BUFFER, 0);
        }
        memory
            .write_bytes(output, &bytes)
            .map_err(ExitReason::MemoryFault)?;
        if used_default != 0 {
            let used = u32::from(!unrepresentable.is_empty());
            memory
                .write_u32(used_default, used)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(bytes.len() as i32)
    }

    pub fn get_ansi_codepage(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(crate::services::codepages::ansi_page())
    }

    pub fn get_oem_codepage(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(crate::services::codepages::oem_page())
    }

    pub fn is_valid_codepage(_context: &mut ServiceContext<'_>, codepage: u32) -> Outcome<bool> {
        // `CP_ACP` and `CP_OEMCP` are pseudo-values, not pages: only the
        // numbers themselves answer true.
        Ok(matches!(
            codepage,
            1252 | 437 | crate::services::codepages::CP_UTF8
        ))
    }

    pub fn get_codepage_info(
        context: &mut ServiceContext<'_>,
        codepage: u32,
        info: OutPtr<18>,
    ) -> Outcome<bool> {
        use crate::services::codepages as pages;
        let state = context.state;
        let teb = context.teb;
        let valid = matches!(codepage, 1252 | 437 | pages::CP_UTF8);
        if !valid || info.0 == 0 {
            return state.fail(teb, pages::ERROR_INVALID_PARAMETER, false);
        }
        // CPINFO: the widest character in bytes, the default character as a
        // byte pair, and twelve zero lead-byte entries (none of these pages
        // is a lead-byte page, UTF-8 included).
        let widest = match codepage {
            pages::CP_UTF8 => 4u32,
            _ => 1,
        };
        let mut bytes = [0u8; 18];
        bytes[..4].copy_from_slice(&widest.to_le_bytes());
        bytes[4] = b'?';
        if info.store(context.memory, bytes).is_err() {
            return state.fail(teb, ERROR_NOACCESS, false);
        }
        Ok(true)
    }

    // The locale identity: `services/locale.rs` says why one locale and
    // what it answers.

    pub fn get_system_default_lcid(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(crate::services::locale::EN_US)
    }

    pub fn get_user_default_lcid(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(crate::services::locale::EN_US)
    }

    pub fn get_user_default_langid(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(crate::services::locale::EN_US)
    }

    pub fn get_user_default_locale_name(
        context: &mut ServiceContext<'_>,
        output: u64,
        count: i32,
    ) -> Outcome<i32> {
        let state = context.state;
        let teb = context.teb;
        let name = crate::services::locale::EN_US_NAME;
        if count == 0 {
            return Ok(name.len() as i32);
        }
        if output == 0 || (count as usize) < name.len() {
            return state.fail(teb, ERROR_INSUFFICIENT_BUFFER, 0);
        }
        let memory = &mut *context.memory;
        for (index, &unit) in name.iter().enumerate() {
            memory
                .write_u16(output + index as u64 * 2, unit)
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(name.len() as i32)
    }

    /// The shared body of the locale-info forms: the string for a type, or
    /// the failure. `wide` chooses the output encoding; every string of the
    /// table is ASCII, so the narrow form is the same bytes.
    fn locale_info(
        context: &mut ServiceContext<'_>,
        kind: u32,
        output: u64,
        count: i32,
        wide: bool,
    ) -> Outcome<i32> {
        let state = context.state;
        let teb = context.teb;
        let Some(text) = crate::services::locale::lookup(kind) else {
            return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
        };
        let len = text.len() + 1;
        if count == 0 {
            return Ok(len as i32);
        }
        if output == 0 || (count as usize) < len {
            return state.fail(teb, ERROR_INSUFFICIENT_BUFFER, 0);
        }
        let memory = &mut *context.memory;
        if wide {
            for (index, &byte) in text.as_bytes().iter().chain(&[0]).enumerate() {
                memory
                    .write_u16(output + index as u64 * 2, u16::from(byte))
                    .map_err(ExitReason::MemoryFault)?;
            }
        } else {
            memory
                .write_bytes(output, text.as_bytes())
                .map_err(ExitReason::MemoryFault)?;
            memory
                .write_bytes(output + text.len() as u64, &[0])
                .map_err(ExitReason::MemoryFault)?;
        }
        Ok(len as i32)
    }

    pub fn get_locale_info_w(
        context: &mut ServiceContext<'_>,
        lcid: u32,
        kind: u32,
        output: u64,
        count: i32,
    ) -> Outcome<i32> {
        if lcid != crate::services::locale::EN_US {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        locale_info(context, kind, output, count, true)
    }

    pub fn get_locale_info_a(
        context: &mut ServiceContext<'_>,
        lcid: u32,
        kind: u32,
        output: u64,
        count: i32,
    ) -> Outcome<i32> {
        if lcid != crate::services::locale::EN_US {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        locale_info(context, kind, output, count, false)
    }

    pub fn get_locale_info_ex(
        context: &mut ServiceContext<'_>,
        name: u64,
        kind: u32,
        output: u64,
        count: i32,
    ) -> Outcome<i32> {
        let state = context.state;
        let teb = context.teb;
        // A null name is the user default, as the page documents; anything
        // else must name the one locale, letter case aside.
        if name != 0 {
            let memory = &mut *context.memory;
            let mut text = Vec::new();
            for index in 0..64u64 {
                let unit = memory
                    .read_u16(name + index * 2)
                    .map_err(ExitReason::MemoryFault)?;
                if unit == 0 {
                    break;
                }
                text.push(unit as u8);
            }
            if !text.is_empty() && !text.eq_ignore_ascii_case(b"en-us") {
                return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
            }
        }
        locale_info(context, kind, output, count, true)
    }

    // String comparison, classification and case mapping for the one locale,
    // package 7 third part of #459. `services/compare.rs` documents the scope.

    fn read_narrow_string(
        memory: &crate::memory::GuestMemory,
        address: u64,
    ) -> Result<Vec<u8>, crate::cpu::ExitReason> {
        let mut bytes = Vec::new();
        for index in 0..65536u64 {
            let byte = memory
                .read_u8(address + index)
                .map_err(crate::cpu::ExitReason::MemoryFault)?;
            if byte == 0 {
                return Ok(bytes);
            }
            bytes.push(byte);
        }
        Err(crate::cpu::ExitReason::MemoryFault(
            crate::memory::MemoryError::UnmappedAddress(address),
        ))
    }

    fn read_wide_units(
        memory: &crate::memory::GuestMemory,
        address: u64,
        count: i32,
    ) -> Result<Vec<u16>, crate::cpu::ExitReason> {
        // count -1 reads up to and excluding the terminator; a positive count
        // reads exactly that many units, including embedded nulls.
        let mut units = Vec::new();
        if count < 0 {
            for index in 0..32768u64 {
                let unit = memory
                    .read_u16(address + index * 2)
                    .map_err(crate::cpu::ExitReason::MemoryFault)?;
                if unit == 0 {
                    return Ok(units);
                }
                units.push(unit);
            }
            return Err(crate::cpu::ExitReason::MemoryFault(
                crate::memory::MemoryError::UnmappedAddress(address),
            ));
        }
        for index in 0..count as u64 {
            units.push(
                memory
                    .read_u16(address + index * 2)
                    .map_err(crate::cpu::ExitReason::MemoryFault)?,
            );
        }
        Ok(units)
    }

    fn read_narrow_counted(
        memory: &crate::memory::GuestMemory,
        address: u64,
        count: i32,
    ) -> Result<Vec<u8>, crate::cpu::ExitReason> {
        if count < 0 {
            return read_narrow_string(memory, address);
        }
        let mut bytes = Vec::new();
        for index in 0..count as u64 {
            bytes.push(
                memory
                    .read_u8(address + index)
                    .map_err(crate::cpu::ExitReason::MemoryFault)?,
            );
        }
        Ok(bytes)
    }

    pub fn lstrlen_a(context: &mut ServiceContext<'_>, text: u64) -> Outcome<i32> {
        let bytes = read_narrow_string(context.memory, text)?;
        Ok(bytes.len() as i32)
    }

    pub fn lstrlen_w(context: &mut ServiceContext<'_>, text: u64) -> Outcome<i32> {
        let units = read_wide_units(context.memory, text, -1)?;
        Ok(units.len() as i32)
    }

    pub fn lstrcpy_a(
        context: &mut ServiceContext<'_>,
        destination: u64,
        source: u64,
    ) -> Outcome<u64> {
        let memory = &mut *context.memory;
        let mut bytes = read_narrow_string(memory, source)?;
        bytes.push(0);
        memory
            .write_bytes(destination, &bytes)
            .map_err(crate::cpu::ExitReason::MemoryFault)?;
        Ok(destination)
    }

    pub fn lstrcmp_a(context: &mut ServiceContext<'_>, left: u64, right: u64) -> Outcome<i32> {
        let a = read_narrow_string(context.memory, left)?;
        let b = read_narrow_string(context.memory, right)?;
        Ok(compare::compare_bytes(&a, &b, false))
    }

    pub fn lstrcmpi_a(context: &mut ServiceContext<'_>, left: u64, right: u64) -> Outcome<i32> {
        let a = read_narrow_string(context.memory, left)?;
        let b = read_narrow_string(context.memory, right)?;
        Ok(compare::compare_bytes(&a, &b, true))
    }

    const NORM_IGNORECASE: u32 = 0x1;
    const CSTR_LESS_THAN: i32 = 1;
    const CSTR_EQUAL: i32 = 2;
    // Flags beyond the case fold are refused rather than silently ignored:
    // the module documents that Xenora compares in byte order, not the
    // linguistic order of Windows en-US.

    fn compare_string(
        context: &mut ServiceContext<'_>,
        wide: bool,
        flags: u32,
        left: u64,
        left_count: i32,
        right: u64,
        right_count: i32,
    ) -> Outcome<i32> {
        if flags & !NORM_IGNORECASE != 0 {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        let fold = flags & NORM_IGNORECASE != 0;
        let order = if wide {
            let a = read_wide_units(context.memory, left, left_count)?;
            let b = read_wide_units(context.memory, right, right_count)?;
            let narrow = |units: &[u16]| -> Vec<u8> {
                units
                    .iter()
                    .map(|unit| {
                        let byte = u8::try_from(*unit).unwrap_or(0xff);
                        if fold { compare::fold(byte) } else { byte }
                    })
                    .collect()
            };
            compare::compare_bytes(&narrow(&a), &narrow(&b), false)
        } else {
            let a = read_narrow_counted(context.memory, left, left_count)?;
            let b = read_narrow_counted(context.memory, right, right_count)?;
            compare::compare_bytes(&a, &b, fold)
        };
        Ok(if order < 0 {
            CSTR_LESS_THAN
        } else if order > 0 {
            CSTR_LESS_THAN + 2
        } else {
            CSTR_EQUAL
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compare_string_a(
        context: &mut ServiceContext<'_>,
        lcid: u32,
        flags: u32,
        left: u64,
        left_count: i32,
        right: u64,
        right_count: i32,
    ) -> Outcome<i32> {
        if lcid != 0x0409 {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        compare_string(context, false, flags, left, left_count, right, right_count)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn compare_string_w(
        context: &mut ServiceContext<'_>,
        lcid: u32,
        flags: u32,
        left: u64,
        left_count: i32,
        right: u64,
        right_count: i32,
    ) -> Outcome<i32> {
        if lcid != 0x0409 {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        compare_string(context, true, flags, left, left_count, right, right_count)
    }

    pub fn get_string_type_w(
        context: &mut ServiceContext<'_>,
        kind: u32,
        source: u64,
        count: i32,
        output: u64,
    ) -> Outcome<bool> {
        // CT_CTYPE1 is 1. The numeric and locale-specific type 2 and 3 tables
        // are refused rather than answered wrong; compare.rs documents it.
        if kind != 1 {
            return context
                .state
                .fail(context.teb, ERROR_INVALID_PARAMETER, false);
        }
        let memory = &mut *context.memory;
        let units = read_wide_units(memory, source, count)?;
        let total = if count < 0 {
            units.len() + 1
        } else {
            units.len()
        };
        for index in 0..total {
            let unit = units.get(index).copied().unwrap_or(0);
            memory
                .write_u16(output + index as u64 * 2, compare::ctype1(unit))
                .map_err(crate::cpu::ExitReason::MemoryFault)?;
        }
        Ok(true)
    }

    const LCMAP_LOWERCASE: u32 = 0x100;
    const LCMAP_UPPERCASE: u32 = 0x200;

    #[allow(clippy::too_many_arguments)]
    pub fn lcmap_string_w(
        context: &mut ServiceContext<'_>,
        lcid: u32,
        flags: u32,
        source: u64,
        count: i32,
        destination: u64,
        destination_count: i32,
    ) -> Outcome<i32> {
        if lcid != 0x0409 {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        // Exactly one case operation, nothing else: sort keys, the byte
        // reversal and the linguistic modifiers are refused. compare.rs
        // documents that only ASCII letters change.
        if flags != LCMAP_UPPERCASE && flags != LCMAP_LOWERCASE {
            return context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        let memory = &mut *context.memory;
        let units = read_wide_units(memory, source, count)?;
        let total = if count < 0 {
            units.len() + 1
        } else {
            units.len()
        };
        let mapped: Vec<u16> = (0..total)
            .map(|index| {
                let unit = units.get(index).copied().unwrap_or(0);
                match u8::try_from(unit) {
                    Ok(byte) if flags == LCMAP_UPPERCASE => u16::from(byte.to_ascii_uppercase()),
                    Ok(byte) => u16::from(byte.to_ascii_lowercase()),
                    Err(_) => unit,
                }
            })
            .collect();
        if destination_count == 0 {
            return Ok(total as i32);
        }
        if (destination_count as usize) < total {
            return context
                .state
                .fail(context.teb, ERROR_INSUFFICIENT_BUFFER, 0);
        }
        for (index, unit) in mapped.iter().enumerate() {
            memory
                .write_u16(destination + index as u64 * 2, *unit)
                .map_err(crate::cpu::ExitReason::MemoryFault)?;
        }
        Ok(total as i32)
    }

    // The console identity and the standard handles, package 8 first part of
    // #459. `services/console.rs` documents the scope.

    pub fn get_std_handle(context: &mut ServiceContext<'_>, kind: u32) -> Outcome<u64> {
        match context.state.console().handle(kind) {
            Some(handle) => Ok(handle),
            None => context.state.fail(context.teb, ERROR_INVALID_PARAMETER, 0),
        }
    }

    pub fn set_std_handle(
        context: &mut ServiceContext<'_>,
        kind: u32,
        handle: u64,
    ) -> Outcome<bool> {
        let mut console = context.state.console();
        console.initialize();
        match kind {
            console::STD_INPUT => console.input_handle = Some(handle),
            console::STD_OUTPUT => console.output_handle = Some(handle),
            console::STD_ERROR => console.error_handle = Some(handle),
            _ => {
                return context
                    .state
                    .fail(context.teb, ERROR_INVALID_PARAMETER, false);
            }
        }
        Ok(true)
    }

    pub fn get_console_cp(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(65001)
    }

    pub fn get_console_output_cp(_context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(65001)
    }

    fn console_device(_context: &ServiceContext<'_>, handle: u64) -> Option<bool> {
        // Some(false) output, Some(true) error, None input or unknown. The
        // match is on the fixed console devices, not on what the standard
        // handles were redirected to: redirecting output to the error device
        // writes to the error capture, and redirecting it to a value that is
        // no console at all fails, as it does on Windows.
        if handle == console::OUTPUT_HANDLE {
            Some(false)
        } else if handle == console::ERROR_HANDLE {
            Some(true)
        } else {
            None
        }
    }

    pub fn get_console_mode(
        context: &mut ServiceContext<'_>,
        handle: u64,
        output: u64,
    ) -> Outcome<bool> {
        let memory = &mut *context.memory;
        let mut console = context.state.console();
        console.initialize();
        // The modes belong to the console devices themselves, like the
        // writes: redirection does not move them.
        let known = handle == console::INPUT_HANDLE
            || handle == console::OUTPUT_HANDLE
            || handle == console::ERROR_HANDLE;
        if !known {
            return context.state.fail(context.teb, ERROR_INVALID_HANDLE, false);
        }
        let mode = if handle == console::INPUT_HANDLE {
            console.input_mode
        } else {
            console.output_mode
        };
        memory
            .write_u32(output, mode)
            .map_err(crate::cpu::ExitReason::MemoryFault)?;
        Ok(true)
    }

    pub fn set_console_mode(
        context: &mut ServiceContext<'_>,
        handle: u64,
        mode: u32,
    ) -> Outcome<bool> {
        let mut console = context.state.console();
        console.initialize();
        let known = handle == console::INPUT_HANDLE
            || handle == console::OUTPUT_HANDLE
            || handle == console::ERROR_HANDLE;
        if !known {
            return context.state.fail(context.teb, ERROR_INVALID_HANDLE, false);
        }
        if handle == console::INPUT_HANDLE {
            console.input_mode = mode;
        } else {
            console.output_mode = mode;
        }
        Ok(true)
    }

    fn write_console(
        context: &mut ServiceContext<'_>,
        handle: u64,
        bytes: &[u8],
        characters: u32,
        written: u64,
    ) -> Outcome<bool> {
        let error = match console_device(context, handle) {
            Some(error) => error,
            None => return context.state.fail(context.teb, ERROR_INVALID_HANDLE, false),
        };
        let memory = &mut *context.memory;
        context.state.crt_streams().console_write(error, bytes);
        if written != 0 {
            memory
                .write_u32(written, characters)
                .map_err(crate::cpu::ExitReason::MemoryFault)?;
        }
        Ok(true)
    }

    pub fn write_console_a(
        context: &mut ServiceContext<'_>,
        handle: u64,
        text: u64,
        characters: u32,
        written: u64,
        reserved: u64,
    ) -> Outcome<bool> {
        if reserved != 0 {
            return context
                .state
                .fail(context.teb, ERROR_INVALID_PARAMETER, false);
        }
        let mut bytes = Vec::new();
        for index in 0..characters as u64 {
            bytes.push(
                context
                    .memory
                    .read_u8(text + index)
                    .map_err(crate::cpu::ExitReason::MemoryFault)?,
            );
        }
        write_console(context, handle, &bytes, characters, written)
    }

    pub fn write_console_w(
        context: &mut ServiceContext<'_>,
        handle: u64,
        text: u64,
        characters: u32,
        written: u64,
        reserved: u64,
    ) -> Outcome<bool> {
        if reserved != 0 {
            return context
                .state
                .fail(context.teb, ERROR_INVALID_PARAMETER, false);
        }
        let mut units = Vec::new();
        for index in 0..characters as u64 {
            units.push(
                context
                    .memory
                    .read_u16(text + index * 2)
                    .map_err(crate::cpu::ExitReason::MemoryFault)?,
            );
        }
        // Wide to UTF-8: the console code page is UTF-8, so the conversion
        // is the encoding, not a code page mapping.
        let text = String::from_utf16_lossy(&units);
        write_console(context, handle, text.as_bytes(), characters, written)
    }

    pub fn read_console_a(
        context: &mut ServiceContext<'_>,
        handle: u64,
        buffer: u64,
        size: u32,
        read: u64,
        control: u64,
    ) -> Outcome<bool> {
        if control != 0 {
            return context
                .state
                .fail(context.teb, ERROR_INVALID_PARAMETER, false);
        }
        if handle != console::INPUT_HANDLE {
            return context.state.fail(context.teb, ERROR_INVALID_HANDLE, false);
        }
        let memory = &mut *context.memory;
        let line = context.state.crt_streams().console_line(size as usize);
        let length = line.len() as u32;
        if length != 0 {
            memory
                .write_bytes(buffer, &line)
                .map_err(crate::cpu::ExitReason::MemoryFault)?;
        }
        if read != 0 {
            memory
                .write_u32(read, length)
                .map_err(crate::cpu::ExitReason::MemoryFault)?;
        }
        Ok(true)
    }

    // What a hosted C program's startup code hands to `main` (#392).
    // `crt_arguments.rs` says what is documented, inferred and Xenora's choice.

    pub fn argc_pointer(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        let mut arguments = context.state.crt_arguments();
        arguments.address(context.memory, crt_arguments::Variable::Argc)
    }

    pub fn argv_pointer(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        let mut arguments = context.state.crt_arguments();
        arguments.address(context.memory, crt_arguments::Variable::Argv)
    }

    pub fn environ_pointer(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        let mut arguments = context.state.crt_arguments();
        arguments.address(context.memory, crt_arguments::Variable::Environ)
    }

    pub fn configure_narrow_argv(context: &mut ServiceContext<'_>, mode: i32) -> Outcome<i32> {
        context
            .state
            .crt_arguments()
            .configure(context.memory, mode)
    }

    pub fn initialize_narrow_environment(context: &mut ServiceContext<'_>) -> Outcome<i32> {
        context
            .state
            .crt_arguments()
            .initialize_environment(context.memory)
    }

    pub fn virtual_alloc(
        context: &mut ServiceContext<'_>,
        requested: u64,
        size: u64,
        kind: u32,
        protect: u32,
    ) -> Outcome<u64> {
        let state = context.state;
        if size == 0 {
            return state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        }
        if kind != MEM_COMMIT_RESERVE {
            return state.fail(context.teb, ERROR_NOT_SUPPORTED, 0);
        }
        // Executable allocations require guest translation cache invalidation.
        let permissions = match protect {
            0x01 => MemoryPermissions::NO_ACCESS,
            0x02 => MemoryPermissions::READ_ONLY,
            0x04 => MemoryPermissions::READ_WRITE,
            _ => return state.fail(context.teb, ERROR_INVALID_PARAMETER, 0),
        };
        let base = requested & !(ALLOCATION_GRANULARITY - 1);
        if requested != 0 && base == 0 {
            return state.fail(context.teb, ERROR_INVALID_ADDRESS, 0);
        }
        let Some(size) = size
            .checked_add(requested - base)
            .and_then(|span| span.checked_next_multiple_of(PAGE_SIZE))
            .and_then(|span| usize::try_from(span).ok())
        else {
            return state.fail(context.teb, ERROR_INVALID_PARAMETER, 0);
        };
        match context
            .memory
            .map_virtual_allocation(base, size, permissions)
        {
            Ok(address) => Ok(address),
            Err(MemoryError::OverlappingRegion { .. } | MemoryError::Overflow) => {
                state.fail(context.teb, ERROR_INVALID_ADDRESS, 0)
            }
            Err(_) => state.fail(context.teb, ERROR_NOT_ENOUGH_MEMORY, 0),
        }
    }

    pub fn virtual_free(
        context: &mut ServiceContext<'_>,
        address: u64,
        size: u64,
        kind: u32,
    ) -> Outcome<bool> {
        let state = context.state;
        if kind != MEM_RELEASE {
            return state.fail(context.teb, ERROR_NOT_SUPPORTED, false);
        }
        if size != 0 {
            return state.fail(context.teb, ERROR_INVALID_PARAMETER, false);
        }
        if !context.memory.release_virtual_allocation(address) {
            return state.fail(context.teb, ERROR_INVALID_ADDRESS, false);
        }
        Ok(true)
    }

    // Stubs (#388). A program built with the mingw-w64 runtime imports these
    // three and does not call them on a normal run: the first two serve its
    // runtime pseudo-relocations and the third is the language handler Windows
    // calls while dispatching a structured exception. None is modelled, so a
    // call stops the guest with the runtime-limit fault. Returning a made-up
    // result would let the program run on with wrong state.

    pub fn virtual_protect(
        _: &mut ServiceContext<'_>,
        _address: u64,
        _size: u64,
        _protect: u32,
        _old: OutPtr<4>,
    ) -> Outcome<bool> {
        Err(RUNTIME_LIMIT)
    }

    pub fn virtual_query(
        _: &mut ServiceContext<'_>,
        _address: u64,
        _information: OutPtr<48>,
        _length: u64,
    ) -> Outcome<u64> {
        Err(RUNTIME_LIMIT)
    }

    /// The guest MXCSR is not visible to a service, so results use the default
    /// rounding and raise no guest flags; the registry marks them partial.
    const DEFAULT_MXCSR: u32 = crate::cpu::Mxcsr::RESET;

    pub fn sqrt(_: &mut ServiceContext<'_>, x: Float64) -> Outcome<Float64> {
        let control = Control::from_mxcsr(DEFAULT_MXCSR);
        Ok(Float64(square_root(F64, x.0, &control).bits))
    }

    pub fn sqrtf(_: &mut ServiceContext<'_>, x: Float32) -> Outcome<Float32> {
        let control = Control::from_mxcsr(DEFAULT_MXCSR);
        let bits = square_root(F32, u64::from(x.0), &control).bits;
        Ok(Float32(bits as u32))
    }

    pub fn fabs(_: &mut ServiceContext<'_>, x: Float64) -> Outcome<Float64> {
        Ok(Float64(x.0 & !(1 << 63)))
    }

    pub fn fabsf(_: &mut ServiceContext<'_>, x: Float32) -> Outcome<Float32> {
        Ok(Float32(x.0 & !(1 << 31)))
    }

    // What a C program's startup code configures (#382). Each value is
    // recorded and returned; `crt_startup.rs` says what does not act on it.

    pub fn set_app_type(context: &mut ServiceContext<'_>, kind: i32) -> Outcome<()> {
        context.state.crt_startup().set_app_type(kind);
        Ok(())
    }

    pub fn set_new_mode(context: &mut ServiceContext<'_>, mode: i32) -> Outcome<i32> {
        let previous = context.state.crt_startup().set_new_mode(mode);
        match previous {
            Some(previous) => Ok(previous),
            None => {
                crt::set_errno(context.memory, context.teb, crt::EINVAL)?;
                Ok(-1)
            }
        }
    }

    pub fn configure_thread_locale(context: &mut ServiceContext<'_>, request: i32) -> Outcome<i32> {
        let previous = context.state.crt_startup().configure_thread_locale(request);
        match previous {
            Some(previous) => Ok(previous),
            None => {
                crt::set_errno(context.memory, context.teb, crt::EINVAL)?;
                Ok(-1)
            }
        }
    }

    pub fn set_user_math_error(context: &mut ServiceContext<'_>, handler: u64) -> Outcome<()> {
        context.state.crt_startup().set_user_math_error(handler);
        Ok(())
    }

    // The legacy CRT's way into the same startup state (#632). `__getmainargs`
    // is how the statically linked startup of a msvcrt-linked program reaches
    // the arguments; the answers are the same table the modern configuration
    // builds, copied through the caller's pointers.

    pub fn getmainargs(
        context: &mut ServiceContext<'_>,
        argc: u64,
        argv: u64,
        envp: u64,
        dowildcard: i32,
        _start_info: u64,
    ) -> Outcome<i32> {
        if argc == 0 || argv == 0 || envp == 0 {
            crt::set_errno(context.memory, context.teb, crt::EINVAL)?;
            return Ok(-1);
        }
        let fault = crate::cpu::ExitReason::MemoryFault;
        let mut arguments = context.state.crt_arguments();
        // A nonzero `dowildcard` asks for wildcard expansion. Xenora does not
        // expand either way, as `_configure_narrow_argv` already documents:
        // the two modes build the same table.
        let mode = if dowildcard == 0 { 1 } else { 2 };
        arguments.configure(context.memory, mode)?;
        arguments.initialize_environment(context.memory)?;
        let count_address = arguments.address(context.memory, crt_arguments::Variable::Argc)?;
        let argv_address = arguments.address(context.memory, crt_arguments::Variable::Argv)?;
        let environ_address =
            arguments.address(context.memory, crt_arguments::Variable::Environ)?;
        let count = context.memory.read_u32(count_address).map_err(fault)?;
        let table = context.memory.read_u64(argv_address).map_err(fault)?;
        let environment = context.memory.read_u64(environ_address).map_err(fault)?;
        context.memory.write_u32(argc, count).map_err(fault)?;
        context.memory.write_u64(argv, table).map_err(fault)?;
        context.memory.write_u64(envp, environment).map_err(fault)?;
        Ok(0)
    }

    /// The legacy CRT's largest multibyte character in the C locale: one
    /// byte, coherent with the byte-oriented narrow conversions of the C
    /// locale documented in `crt_wide.rs`.
    pub fn mb_cur_max_legacy(_: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(1)
    }

    /// `_amsg_exit` ends the process after a runtime error. Partial: the
    /// message the CRT writes is not modelled.
    pub fn amsg_exit(context: &mut ServiceContext<'_>, code: i32) -> Outcome<()> {
        Err(crate::cpu::ExitReason::ProcessExit(
            context.state.exit_process(code as u32),
        ))
    }

    pub fn set_invalid_parameter_handler(
        context: &mut ServiceContext<'_>,
        handler: u64,
    ) -> Outcome<u64> {
        Ok(context
            .state
            .crt_startup()
            .set_invalid_parameter_handler(handler))
    }

    pub fn file_mode_pointer(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        context
            .state
            .crt_startup()
            .file_mode_address(context.memory, 0)
    }

    pub fn commit_mode_pointer(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        context
            .state
            .crt_startup()
            .file_mode_address(context.memory, 1)
    }

    pub fn signal(context: &mut ServiceContext<'_>, signal: i32, handler: u64) -> Outcome<u64> {
        let previous = context.state.crt_startup().signal(signal, handler);
        match previous {
            Some(previous) => Ok(previous),
            None => {
                crt::set_errno(context.memory, context.teb, crt::EINVAL)?;
                // SIG_ERR is (void (*)(int))-1.
                Ok(u64::MAX)
            }
        }
    }

    pub fn set_unhandled_exception_filter(
        context: &mut ServiceContext<'_>,
        filter: u64,
    ) -> Outcome<u64> {
        Ok(context
            .state
            .crt_startup()
            .set_unhandled_exception_filter(filter))
    }

    // The Universal C runtime services that a C++ guest's runtime imports.
    // Their implementations are in `crt.rs` and `crt_streams.rs`.
    pub use super::crt::{_lock_file, _unlock_file, fputc, localeconv, mbrtowc, strerror, wcrtomb};

    // The C time services of package 5 of #459. `crt_clock.rs` documents
    // the UTC clock they share.
    pub fn _gmtime64(context: &mut ServiceContext<'_>, time: u64) -> Outcome<u64> {
        context
            .state
            .crt_clock()
            .break_time(context.memory, context.teb, time)
    }

    pub fn _mktime64(context: &mut ServiceContext<'_>, tm: u64) -> Outcome<u64> {
        context
            .state
            .crt_clock()
            .compose_time(context.memory, context.teb, tm)
    }

    pub fn asctime(context: &mut ServiceContext<'_>, tm: u64) -> Outcome<u64> {
        context
            .state
            .crt_clock()
            .ascii_time(context.memory, context.teb, tm)
    }

    pub fn _ctime64(context: &mut ServiceContext<'_>, time: u64) -> Outcome<u64> {
        context
            .state
            .crt_clock()
            .ascii_clock(context.memory, context.teb, time)
    }

    pub fn asctime_s(
        context: &mut ServiceContext<'_>,
        buffer: u64,
        size: u64,
        tm: u64,
    ) -> Outcome<u32> {
        context
            .state
            .crt_clock()
            .ascii_time_s(context.memory, context.teb, buffer, size, tm)
            .map(|value| value as u32)
    }

    pub fn _ctime64_s(
        context: &mut ServiceContext<'_>,
        buffer: u64,
        size: u64,
        time: u64,
    ) -> Outcome<u32> {
        context
            .state
            .crt_clock()
            .ascii_clock_s(context.memory, context.teb, buffer, size, time)
            .map(|value| value as u32)
    }

    pub fn strftime(
        context: &mut ServiceContext<'_>,
        out: u64,
        size: u64,
        format: u64,
        tm: u64,
    ) -> Outcome<u64> {
        context
            .state
            .crt_clock()
            .format_time(context.memory, context.teb, out, size, format, tm)
    }

    pub fn clock(context: &mut ServiceContext<'_>) -> Outcome<u64> {
        // `clock` counts in units of CLOCKS_PER_SEC, 1000; the performance
        // count is in 100-nanosecond ticks, so ten thousand of them are
        // one of its units.
        Ok(context.state.clock.performance_count() / 10_000)
    }

    pub fn _difftime64(
        _context: &mut ServiceContext<'_>,
        time1: u64,
        time2: u64,
    ) -> Outcome<Float64> {
        Ok(Float64(
            (time1 as i64 as f64 - time2 as i64 as f64).to_bits(),
        ))
    }

    pub fn file_time_to_local_file_time(
        context: &mut ServiceContext<'_>,
        input: u64,
        output: u64,
    ) -> Outcome<u32> {
        // The machine's local time is UTC, so the conversion is the
        // identity. Failure for an unrepresentable time cannot occur.
        let time = context.memory.read_u64(input).map_err(crt_streams::fault)?;
        context
            .memory
            .write_u64(output, time)
            .map_err(crt_streams::fault)?;
        Ok(1)
    }

    // The wide-character conversions and classification of package 6 of
    // #459. `crt_wide.rs` documents the C locale's identity encoding they
    // convert in.
    pub fn mbtowc(
        context: &mut ServiceContext<'_>,
        wide: u64,
        text: u64,
        count: u64,
    ) -> Outcome<i32> {
        crt_wide::multibyte_to_wide(context.memory, context.teb, wide, text, count)
            .map(|value| value as i32)
    }

    pub fn wctomb(context: &mut ServiceContext<'_>, text: u64, wide: u32) -> Outcome<i32> {
        crt_wide::wide_to_multibyte(context.memory, context.teb, text, u64::from(wide))
            .map(|value| value as i32)
    }

    pub fn mblen(context: &mut ServiceContext<'_>, text: u64, count: u64) -> Outcome<i32> {
        crt_wide::multibyte_length(context.memory, context.teb, text, count)
            .map(|value| value as i32)
    }

    pub fn mbstowcs(
        context: &mut ServiceContext<'_>,
        wide: u64,
        text: u64,
        count: u64,
    ) -> Outcome<u64> {
        crt_wide::multibyte_string_to_wide(context.memory, context.teb, wide, text, count)
    }

    pub fn wcstombs(
        context: &mut ServiceContext<'_>,
        text: u64,
        wide: u64,
        count: u64,
    ) -> Outcome<u64> {
        crt_wide::wide_string_to_multibyte(context.memory, context.teb, text, wide, count)
    }

    pub fn mbsrtowcs(
        context: &mut ServiceContext<'_>,
        wide: u64,
        source: u64,
        count: u64,
        _state: u64,
    ) -> Outcome<u64> {
        crt_wide::mbsrtowcs(context.memory, context.teb, wide, source, count)
    }

    pub fn wcsrtombs(
        context: &mut ServiceContext<'_>,
        text: u64,
        source: u64,
        count: u64,
        _state: u64,
    ) -> Outcome<u64> {
        crt_wide::wcsrtombs(context.memory, context.teb, text, source, count)
    }

    pub fn iswalpha(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<i32> {
        crt_wide::iswalpha(context.memory, context.teb, u64::from(wide)).map(|value| value as i32)
    }

    pub fn iswdigit(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<i32> {
        crt_wide::iswdigit(context.memory, context.teb, u64::from(wide)).map(|value| value as i32)
    }

    pub fn iswalnum(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<i32> {
        crt_wide::iswalnum(context.memory, context.teb, u64::from(wide)).map(|value| value as i32)
    }

    pub fn iswlower(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<i32> {
        crt_wide::iswlower(context.memory, context.teb, u64::from(wide)).map(|value| value as i32)
    }

    pub fn iswupper(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<i32> {
        crt_wide::iswupper(context.memory, context.teb, u64::from(wide)).map(|value| value as i32)
    }

    pub fn iswspace(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<i32> {
        crt_wide::iswspace(context.memory, context.teb, u64::from(wide)).map(|value| value as i32)
    }

    pub fn towlower(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<u32> {
        crt_wide::towlower(context.memory, context.teb, u64::from(wide)).map(|value| value as u32)
    }

    pub fn towupper(context: &mut ServiceContext<'_>, wide: u32) -> Outcome<u32> {
        crt_wide::towupper(context.memory, context.teb, u64::from(wide)).map(|value| value as u32)
    }

    pub fn iswctype(context: &mut ServiceContext<'_>, wide: u32, mask: u32) -> Outcome<i32> {
        crt_wide::iswctype(
            context.memory,
            context.teb,
            u64::from(wide),
            u64::from(mask),
        )
        .map(|value| value as i32)
    }

    pub fn wcsncat(
        context: &mut ServiceContext<'_>,
        start: u64,
        end: u64,
        count: u64,
    ) -> Outcome<u64> {
        crt_wide::wcsncat(context.memory, context.teb, start, end, count)
    }

    pub fn _wcsdup(context: &mut ServiceContext<'_>, text: u64) -> Outcome<u64> {
        crt_wide::wcsdup(context.memory, context.teb, text)
    }

    // The rest of math.h (#468); `crt_math.rs` says which are exact.
    pub use super::crt_math::{
        acos, asin, atan, atan2, atan2f, cbrt, ceil, ceilf, copysign, cos, cosf, cosh, erf, exp,
        exp2, expf, expm1, fdim, floor, floorf, fmax, fmin, fmod, fmodf, frexp, hypot, llround,
        log, log1p, log2, log10, logf, lround, modf, nearbyint, pow, powf, remainder, rint, round,
        roundf, sin, sinf, sinh, tan, tanf, tanh, tgamma, trunc,
    };
}
