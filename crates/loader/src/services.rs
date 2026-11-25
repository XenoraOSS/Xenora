// SPDX-License-Identifier: GPL-3.0-or-later

//! Standard Windows and Xbox Scarlett Host Services Implementation for Xenora X15.
//!
//! Provides verifiable guest implementations of Windows NT and Xbox Scarlett APIs:
//! - Time: `GetSystemTimeAsFileTime` (injected deterministic clock or real UTC)
//! - Process and thread identity and guest process priority boost policy
//! - Error slots: `GetLastError`, `SetLastError`
//! - Virtual memory: `VirtualAlloc`, `VirtualFree`
//! - Xbox physical memory: `XMemAlloc`, `XMemFree`

use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use crate::memory::{ALLOCATION_GRANULARITY, GuestMemory, MemoryError, MemoryPermissions};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
const MAX_THREAD_HANDLES: usize = 1024;
use crate::graphics_service::MAX_GUEST_OBJECTS;
const PAGE_SIZE: u64 = 4096;
const MEM_COMMIT_RESERVE: u32 = 0x3000;
const MEM_RELEASE: u32 = 0x8000;
const ERROR_NOACCESS: u32 = 998;

/// Clock trait for injecting deterministic time in tests or host time in production.
pub trait Clock: Send + Sync {
    /// Return Windows 100-nanosecond intervals since January 1, 1601 (UTC).
    fn now_filetime(&self) -> u64;
}

/// Deterministic test clock.
pub struct MockClock {
    current: AtomicU64,
}

impl MockClock {
    pub fn new(initial_filetime: u64) -> Self {
        Self {
            current: AtomicU64::new(initial_filetime),
        }
    }

    pub fn advance(&self, intervals: u64) {
        self.current.fetch_add(intervals, Ordering::SeqCst);
    }
}

impl Clock for MockClock {
    fn now_filetime(&self) -> u64 {
        self.current.load(Ordering::SeqCst)
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

#[derive(Default)]
struct ThreadState {
    priority: i32,
    last_handle: u64,
    handles: BTreeMap<u64, u32>,
}

impl ThreadState {
    fn check_access(&self, handle: u64, required: u32) -> Result<(), u32> {
        if handle == CURRENT_THREAD {
            return Ok(());
        }
        match self.handles.get(&handle) {
            Some(access) if access & required != 0 => Ok(()),
            Some(_) => Err(ERROR_ACCESS_DENIED),
            None => Err(ERROR_INVALID_HANDLE),
        }
    }
}

/// State container for runtime services.
pub struct ServicesState {
    pub clock: Arc<dyn Clock>,
    pub process_id: u32,
    priority_boost_disabled: AtomicBool,
    pub thread_id: u32,
    thread: Mutex<ThreadState>,
    pub last_error: AtomicU32,
    pub next_alloc_addr: AtomicU64,
    pub graphics: Arc<Mutex<crate::graphics::GraphicsContext>>,
}

impl ServicesState {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            clock,
            priority_boost_disabled: AtomicBool::new(false),
            process_id: 0x4100, // Synthetic process ID
            thread_id: 0x4104,  // Synthetic main thread ID
            thread: Mutex::new(ThreadState::default()),
            last_error: AtomicU32::new(0),
            next_alloc_addr: AtomicU64::new(0x0000_0002_0000_0000), // 8GB guest heap
            graphics: Arc::new(Mutex::new(crate::graphics::GraphicsContext::new())),
        }
    }
    /// The scheduling policy recorded for the synthetic process.
    pub fn priority_boost_disabled(&self) -> bool {
        self.priority_boost_disabled.load(Ordering::SeqCst)
    }

    /// Record a guest error and return the documented failure value.
    fn fail<T>(&self, error: u32, value: T) -> Result<T, ExitReason> {
        self.last_error.store(error, Ordering::SeqCst);
        Ok(value)
    }

    fn is_current_process(&self, handle: u64) -> bool {
        if handle == CURRENT_PROCESS {
            return true;
        }
        self.last_error
            .store(ERROR_INVALID_HANDLE, Ordering::SeqCst);
        false
    }
}

/// Register the services generated from `runtime/services.txt` and the
/// hand-written fixture services.
pub fn register_standard_services(registry: &mut ImportRegistry, state: Arc<ServicesState>) {
    crate::runtime_table::register(registry, &state);

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

        let page_size = 4096;
        let aligned_size = (size + page_size - 1) & !(page_size - 1);
        let alloc_addr = s
            .next_alloc_addr
            .fetch_add(aligned_size as u64, Ordering::SeqCst);

        if mem
            .map_region(alloc_addr, aligned_size, MemoryPermissions::READ_WRITE)
            .is_err()
        {
            return Ok(0);
        }

        Ok(alloc_addr)
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

    let graphics = state.graphics.clone();
    registry.register_with_status(
        "xenora_fixture.dll",
        "GraphicsCallV1",
        ServiceStatus::Implemented,
        move |args, _cpu, memory| {
            let descriptor = args.get(0, memory).map_err(ExitReason::MemoryFault)?;
            let bytes = args.get(1, memory).map_err(ExitReason::MemoryFault)?;
            let result = args.get(2, memory).map_err(ExitReason::MemoryFault)?;
            if args.get(3, memory).map_err(ExitReason::MemoryFault)? != 0 {
                return Ok(crate::graphics_service::STATUS_INVALID_CALL);
            }
            let mut context = graphics
                .lock()
                .map_err(|_| ExitReason::IllegalInstruction(0xff))?;
            crate::graphics_service::graphics_call_v1(
                &mut context,
                memory,
                descriptor,
                bytes,
                result,
            )
        },
    );

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
            return Ok(0x80070057); // E_INVALIDARG
        }
        mem.validate_write_range(pp_device, 8)
            .map_err(ExitReason::MemoryFault)?;
        let mut gfx = s_gfx
            .lock()
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
        if gfx.guest_object_count() >= MAX_GUEST_OBJECTS {
            return Ok(0x80004005); // E_FAIL
        }
        match gfx.create_device() {
            Ok(handle) => {
                mem.write_u64(pp_device, handle.to_u64())
                    .map_err(ExitReason::MemoryFault)?;
                Ok(0) // S_OK
            }
            Err(_) => Ok(0x80004005), // E_FAIL
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
            return Ok(0x80070057); // E_INVALIDARG
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

        if vp_w > 4096 || vp_h > 4096 {
            return Ok(0x80070057); // E_INVALIDARG
        }
        let pp_out = args.get(3, mem).map_err(ExitReason::MemoryFault)?;
        if pp_out != 0 {
            mem.validate_write_range(pp_out, 8)
                .map_err(ExitReason::MemoryFault)?;
        }
        vtx_addr
            .checked_add(48)
            .ok_or(ExitReason::MemoryFault(MemoryError::Overflow))?;
        let mut vertex_bytes = [0; 48];
        mem.read_bytes(vtx_addr, &mut vertex_bytes)
            .map_err(ExitReason::MemoryFault)?;

        let mut gfx = s_gfx
            .lock()
            .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;

        // Reserve room for device, queue, allocator, list and vertex buffer before
        // creating any of them. All guest imports count the same context objects.
        if gfx.guest_object_count() > MAX_GUEST_OBJECTS - 5 {
            return Ok(0x80004005); // E_FAIL
        }

        let mut temporary = Vec::with_capacity(5);
        let submission = (|| {
            // Build and execute guest command list through GraphicsContext
            let dev = gfx
                .create_device()
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            temporary.push(dev);
            let queue = gfx
                .create_command_queue(dev, crate::graphics::CommandListType::Direct)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            temporary.push(queue);
            let alloc = gfx
                .create_command_allocator(dev, crate::graphics::CommandListType::Direct)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            temporary.push(alloc);
            let list = gfx
                .create_command_list(dev, alloc, crate::graphics::CommandListType::Direct)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            temporary.push(list);
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
            temporary.push(vb);
            gfx.cmd_set_viewport(list, vp_w, vp_h)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            gfx.cmd_set_render_targets(list, None, clear_color)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            gfx.cmd_set_vertex_buffer(list, vb, 0, 16)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            gfx.cmd_draw_instanced(list, 3, 1, 0, 0)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            gfx.close_command_list(list)
                .map_err(|_| ExitReason::MemoryFault(crate::memory::MemoryError::Overflow))?;
            gfx.execute_command_list(queue, list, mem).map_err(|e| {
                ExitReason::IllegalInstruction(match e {
                    crate::graphics::GraphicsError::NativeMetalRequired => 0xFE,
                    _ => 0xFF,
                })
            })
        })();
        let outcome = match submission {
            Ok(outcome) => outcome,
            Err(error) => {
                gfx.discard_unpublished_guest_objects(&temporary);
                return Err(error);
            }
        };
        // Successful legacy submissions retain their objects, preserving the
        // existing shared-cap contract. Failed submissions retain none.

        if pp_out != 0 {
            mem.write_u64(pp_out, outcome.pixels.len() as u64)
                .map_err(ExitReason::MemoryFault)?;
        }

        Ok(0) // S_OK
    };
    registry.register("xenora_fixture.dll", "SubmitGuestDraw", handler);
    registry.set_status(
        "xenora_fixture.dll",
        "SubmitGuestDraw",
        ServiceStatus::Implemented,
    );
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
        if !context.state.is_current_process(process) {
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
        if !context.state.is_current_process(process) {
            return Ok(false);
        }
        let value = u32::from(context.state.priority_boost_disabled());
        if disabled.store(context.memory, value.to_le_bytes()).is_err() {
            return context.state.fail(ERROR_NOACCESS, false);
        }
        Ok(true)
    }

    pub fn get_current_process(_: &mut ServiceContext<'_>) -> Outcome<u64> {
        Ok(CURRENT_PROCESS)
    }

    pub fn get_process_id(context: &mut ServiceContext<'_>, process: u64) -> Outcome<u32> {
        Ok(if context.state.is_current_process(process) {
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
        Ok(context.state.thread_id)
    }

    pub fn open_thread(
        context: &mut ServiceContext<'_>,
        access: u32,
        inherit: bool,
        id: u32,
    ) -> Outcome<u64> {
        let state = context.state;
        if id != state.thread_id {
            return state.fail(ERROR_INVALID_PARAMETER, 0);
        }
        if inherit || access & !THREAD_SUPPORTED_RIGHTS != 0 {
            return state.fail(ERROR_NOT_SUPPORTED, 0);
        }
        let mut thread = state.thread.lock().expect("guest thread state");
        let Some(handle) = thread
            .last_handle
            .checked_add(4)
            .filter(|_| thread.handles.len() < MAX_THREAD_HANDLES)
        else {
            return state.fail(ERROR_NOT_ENOUGH_MEMORY, 0);
        };
        thread.handles.insert(handle, access);
        thread.last_handle = handle;
        Ok(handle)
    }

    pub fn get_thread_priority(context: &mut ServiceContext<'_>, handle: u64) -> Outcome<i32> {
        let thread = context.state.thread.lock().expect("guest thread state");
        if let Err(error) = thread.check_access(handle, THREAD_QUERY_RIGHTS) {
            return context
                .state
                .fail(error, THREAD_PRIORITY_ERROR_RETURN as i32);
        }
        Ok(thread.priority)
    }

    pub fn set_thread_priority(
        context: &mut ServiceContext<'_>,
        handle: u64,
        priority: i32,
    ) -> Outcome<bool> {
        let state = context.state;
        let mut thread = state.thread.lock().expect("guest thread state");
        if let Err(error) = thread.check_access(handle, THREAD_SET_RIGHTS) {
            return state.fail(error, false);
        }
        if matches!(priority, 0x0001_0000 | 0x0002_0000) {
            return state.fail(ERROR_NOT_SUPPORTED, false);
        }
        if !matches!(priority, -15 | -2..=2 | 15) {
            return state.fail(ERROR_INVALID_PARAMETER, false);
        }
        thread.priority = priority;
        Ok(true)
    }

    pub fn close_handle(context: &mut ServiceContext<'_>, handle: u64) -> Outcome<bool> {
        let closed = matches!(handle, CURRENT_PROCESS | CURRENT_THREAD)
            || context
                .state
                .thread
                .lock()
                .expect("guest thread state")
                .handles
                .remove(&handle)
                .is_some();
        if closed {
            Ok(true)
        } else {
            context.state.fail(ERROR_INVALID_HANDLE, false)
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
        Ok(context.state.last_error.load(Ordering::SeqCst))
    }

    pub fn set_last_error(context: &mut ServiceContext<'_>, code: u32) -> Outcome<()> {
        context.state.last_error.store(code, Ordering::SeqCst);
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
            return state.fail(ERROR_INVALID_PARAMETER, 0);
        }
        if kind != MEM_COMMIT_RESERVE {
            return state.fail(ERROR_NOT_SUPPORTED, 0);
        }
        // Executable allocations require guest translation cache invalidation.
        let permissions = match protect {
            0x01 => MemoryPermissions::NO_ACCESS,
            0x02 => MemoryPermissions::READ_ONLY,
            0x04 => MemoryPermissions::READ_WRITE,
            _ => return state.fail(ERROR_INVALID_PARAMETER, 0),
        };
        let base = requested & !(ALLOCATION_GRANULARITY - 1);
        if requested != 0 && base == 0 {
            return state.fail(ERROR_INVALID_ADDRESS, 0);
        }
        let Some(size) = size
            .checked_add(requested - base)
            .and_then(|span| span.checked_next_multiple_of(PAGE_SIZE))
            .and_then(|span| usize::try_from(span).ok())
        else {
            return state.fail(ERROR_INVALID_PARAMETER, 0);
        };
        match context
            .memory
            .map_virtual_allocation(base, size, permissions)
        {
            Ok(address) => Ok(address),
            Err(MemoryError::OverlappingRegion { .. } | MemoryError::Overflow) => {
                state.fail(ERROR_INVALID_ADDRESS, 0)
            }
            Err(_) => state.fail(ERROR_NOT_ENOUGH_MEMORY, 0),
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
            return state.fail(ERROR_NOT_SUPPORTED, false);
        }
        if size != 0 {
            return state.fail(ERROR_INVALID_PARAMETER, false);
        }
        if !context.memory.release_virtual_allocation(address) {
            return state.fail(ERROR_INVALID_ADDRESS, false);
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
