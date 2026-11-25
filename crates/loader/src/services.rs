// SPDX-License-Identifier: GPL-3.0-or-later

//! Standard Windows and Xbox Scarlett Host Services Implementation for Xenora X15.
//!
//! Provides verifiable guest implementations of Windows NT and Xbox Scarlett APIs:
//! - Time: UTC file time and a monotonic performance counter
//! - Process and thread identity and guest process priority boost policy
//! - Error slots: `GetLastError`, `SetLastError`
//! - Virtual memory: `VirtualAlloc`, `VirtualFree`
//! - Xbox physical memory: `XMemAlloc`, `XMemFree`

mod crt;
mod crt_callbacks;
mod crt_ctype;
mod crt_env;
mod crt_exit;
mod crt_float;
mod crt_heap;
mod crt_integer;
mod crt_locale;
mod crt_safe_string;
mod crt_stdio;
mod crt_time;
mod cxx;
mod events;
mod files;
mod gdk_async;
mod gdk_queue;
mod heap;
pub mod host_files;
mod modules;
mod platform_fixture;
mod process;
mod synch;
pub mod sysinfo;
mod task_fixture;
pub(crate) mod threads;
mod winsock;
pub mod xinput;

use crate::runtime::HResult;

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{ALLOCATION_GRANULARITY, GuestMemory, MemoryError, MemoryPermissions};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

const CURRENT_PROCESS: u64 = u64::MAX;
const CURRENT_THREAD: u64 = u64::MAX - 1;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
const ERROR_NOT_SUPPORTED: u32 = 50;
const ERROR_INVALID_PARAMETER: u32 = 87;
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
}

#[derive(Default)]
struct ObjectState {
    priority: i32,
    last_handle: u64,
    handles: BTreeMap<u64, GuestObject>,
    files: BTreeMap<String, files::SyntheticFile>,
    directories: BTreeSet<String>,
    current_directory: String,
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
    /// Host directories exposed as guest drives; see [`ServicesState::mount`].
    mounts: Vec<host_files::HostMount>,
    objects: Mutex<ObjectState>,
    last_errors: Mutex<BTreeMap<u64, u32>>,
    synch: Mutex<synch::Synch>,
    pub(crate) threads: Mutex<threads::ThreadTable>,
    pub(crate) hooks: Arc<threads::SchedulerHooks>,
    /// Functions registered with `atexit`, run in reverse order by `_cexit`.
    atexit: Mutex<Vec<u64>>,
    /// The status the guest process exited with. Set once.
    exit_status: Mutex<Option<u32>>,
    winsock: Mutex<winsock::Winsock>,
    /// Where XInput readings come from; see [`ServicesState::set_controllers`].
    controllers: Arc<dyn xinput::ControllerSource>,
    xinput: Mutex<xinput::XInput>,
    type_names: Arc<Mutex<cxx::TypeNames>>,
    aligned: Mutex<crt_heap::AlignedBlocks>,
    task_fixture: Mutex<task_fixture::TaskFixture>,
    platform_fixture: Mutex<platform_fixture::PlatformFixture>,
    gdk_queues: Mutex<gdk_queue::QueueHandles>,
    gdk_queue_changed: Condvar,
    gdk_async: Arc<Mutex<gdk_async::AsyncOps>>,
    locale: Mutex<crt_locale::LocaleState>,
    stdio: Mutex<crt_stdio::StdioState>,
    env: Mutex<crt_env::EnvState>,
    /// Seed for the CRT `rand` / `srand` LCG (process-wide, like UCRT).
    crt_rand: AtomicU32,
    winsock_changed: Condvar,
    pub next_alloc_addr: AtomicU64,
    pub graphics: Arc<Mutex<crate::graphics::GraphicsContext>>,
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
            mounts: Vec::new(),
            objects: Mutex::new(ObjectState::default()),
            last_errors: Mutex::default(),
            synch: Mutex::default(),
            threads: Mutex::default(),
            hooks: Arc::default(),
            atexit: Mutex::default(),
            exit_status: Mutex::default(),
            winsock: Mutex::default(),
            controllers: Arc::new(xinput::NoControllers),
            xinput: Mutex::default(),
            type_names: Arc::default(),
            aligned: Mutex::default(),
            task_fixture: Mutex::default(),
            platform_fixture: Mutex::default(),
            gdk_queues: Mutex::default(),
            gdk_queue_changed: Condvar::new(),
            gdk_async: Arc::new(Mutex::default()),
            locale: Mutex::default(),
            stdio: Mutex::default(),
            env: Mutex::default(),
            crt_rand: AtomicU32::new(1),
            winsock_changed: Condvar::new(),
            next_alloc_addr: AtomicU64::new(0x0000_0002_0000_0000), // 8GB guest heap
            graphics: Arc::new(Mutex::new(crate::graphics::GraphicsContext::new())),
        }
    }
    /// Connect a controller source to XInput. Without one, every slot reports
    /// that no controller is connected.
    pub fn set_controllers(&mut self, source: Arc<dyn xinput::ControllerSource>) {
        self.controllers = source;
    }

    /// Expose a host directory to the guest. Each drive letter can be mounted
    /// once. Nothing on the host is reachable unless it is mounted here.
    pub fn mount(&mut self, mount: host_files::HostMount) -> Result<(), host_files::HostMount> {
        if self
            .mounts
            .iter()
            .any(|existing| existing.drive() == mount.drive())
        {
            return Err(mount);
        }
        self.mounts.push(mount);
        Ok(())
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
            .map(|file| file.data().to_vec())
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

/// Register the services generated from `runtime/services.txt` and the
/// hand-written fixture services.
pub fn register_standard_services(registry: &mut ImportRegistry, state: Arc<ServicesState>) {
    crate::runtime_table::register(registry, &state);
    registry.hooks = Some(Arc::clone(&state.hooks));
    threads::register(registry, &state);

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
                Some(GuestObject::Search(_)) => return s.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0),
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
            GuestObject::Search(_) => unreachable!("search handles cannot be duplicated"),
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
    crt::register(registry);
    crt::register_heap(registry);
    crt_heap::register(registry, &state);
    crt_safe_string::register(registry, &state);
    crt::register_errno(registry);
    crt::register_conversions(registry);
    crt::register_rand(registry, &state);
    crt_locale::register(registry, &state);
    crt_ctype::register(registry);
    crt_stdio::register(registry, &state);
    crt_env::register(registry, &state);
    crt_time::register(registry, &state);
    crt_callbacks::register(registry, &state);
    heap::register(registry, &state);

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
    xinput::register(registry, &state);
    cxx::register(registry, &state);
    task_fixture::register(registry, &state);
    platform_fixture::register(registry, &state);
    gdk_queue::register(registry, &state);
    gdk_async::register(registry, &state);
    modules::register(registry, &state);
    process::register(registry, &state);
    crt_exit::register(registry, &state);
    crt_integer::register(registry);
    crt_float::register(registry);
}

/// Implementations behind the generated adapters. A failure the guest can
/// observe sets its last error; `Err` stops the guest.
pub(crate) mod handlers {
    use super::*;
    use crate::runtime::{Float32, Float64, OutPtr, ServiceContext};
    use crate::softfloat::{Control, F32, F64, square_root};

    type Outcome<T> = Result<T, ExitReason>;

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

    pub fn get_last_error(context: &mut ServiceContext<'_>) -> Outcome<u32> {
        Ok(context.state.last_error(context.teb))
    }

    pub fn set_last_error(context: &mut ServiceContext<'_>, code: u32) -> Outcome<()> {
        context.state.set_last_error(context.teb, code);
        Ok(())
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
}
