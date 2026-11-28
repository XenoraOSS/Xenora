// SPDX-License-Identifier: GPL-3.0-or-later

//! Bounded, process-local ANSI windows and synchronous guest callbacks.

use super::{MODULE, MSG_SIZE, MessageQueues, WM_CLOSE, discard_window_messages};
use crate::cpu::ExitReason;
use crate::imports::{
    CallbackStart, CallbackStep, CallbackTask, ImportRegistry, ServiceStatus, Win64Args,
};
use crate::memory::{GuestMemory, PROCESS_HEAP};
use crate::services::{
    ERROR_INVALID_PARAMETER, ERROR_NOACCESS, ERROR_NOT_ENOUGH_MEMORY, ERROR_NOT_SUPPORTED,
    ServicesState,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
const ERROR_CANNOT_FIND_WND_CLASS: u32 = 1407;
const ERROR_WINDOW_OF_OTHER_THREAD: u32 = 1408;
const ERROR_CLASS_ALREADY_EXISTS: u32 = 1410;
const ERROR_CLASS_DOES_NOT_EXIST: u32 = 1411;
const ERROR_CLASS_HAS_WINDOWS: u32 = 1412;
const WINDOW_CLASS_SIZE: usize = 80;
const MAX_CLASS_NAME_BYTES: u64 = 255;
const CLASS_ATOMS: std::ops::Range<u16> = 0xc000..0xc040;
const MAX_WINDOWS: usize = 128;
const FIRST_WINDOW: u64 = 0x5757_0000_0000;
const WS_OVERLAPPEDWINDOW: u32 = 0x00cf_0000;

const WM_CREATE: u64 = 0x0001;
const WM_DESTROY: u64 = 0x0002;
const WM_SHOWWINDOW: u64 = 0x0018;
const WM_NCCREATE: u64 = 0x0081;
const WM_NCDESTROY: u64 = 0x0082;

struct WindowClass {
    name: Vec<u8>,
    instance: u64,
    entry: u64,
}

struct Window {
    atom: u16,
    owner: u32,
    visible: bool,
    showing: bool,
    destroying: bool,
}

#[derive(Default)]
pub(super) struct WindowState {
    classes: BTreeMap<u16, WindowClass>,
    windows: BTreeMap<u64, Window>,
    last_window: u64,
    // An abandoned callback task can retire its guest allocation on the
    // next creation, when guest memory is available again.
    retired_scratch: Vec<u64>,
}

impl WindowState {
    pub(super) fn owner_of(&self, hwnd: u64) -> Option<u32> {
        self.windows.get(&hwnd).map(|window| window.owner)
    }

    fn class_atom(&self, selector: ClassSelector, instance: u64) -> Option<u16> {
        match selector {
            ClassSelector::Atom(atom) => self
                .classes
                .get(&atom)
                .filter(|class| class.instance == instance)
                .map(|_| atom),
            ClassSelector::Name(name) => self.classes.iter().find_map(|(&atom, class)| {
                (class.instance == instance && class.name.eq_ignore_ascii_case(&name))
                    .then_some(atom)
            }),
        }
    }

    fn owned_window(&mut self, hwnd: u64, owner: u32) -> Result<&mut Window, u32> {
        let window = self
            .windows
            .get_mut(&hwnd)
            .ok_or(ERROR_INVALID_WINDOW_HANDLE)?;
        if window.owner != owner {
            return Err(ERROR_WINDOW_OF_OTHER_THREAD);
        }
        Ok(window)
    }
}

enum ClassSelector {
    Atom(u16),
    Name(Vec<u8>),
}

fn class_name(memory: &GuestMemory, address: u64) -> Result<Vec<u8>, u32> {
    if address == 0 {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let mut name = Vec::new();
    for offset in 0..=MAX_CLASS_NAME_BYTES {
        let byte = memory
            .read_u8(address.checked_add(offset).ok_or(ERROR_NOACCESS)?)
            .map_err(|_| ERROR_NOACCESS)?;
        match byte {
            0 if !name.is_empty() => return Ok(name),
            1..=127 => name.push(byte),
            _ => return Err(ERROR_INVALID_PARAMETER),
        }
    }
    Err(ERROR_INVALID_PARAMETER)
}

fn class_selector(memory: &GuestMemory, address: u64) -> Result<ClassSelector, u32> {
    if (1..=u64::from(u16::MAX)).contains(&address) {
        return Ok(ClassSelector::Atom(address as u16));
    }
    class_name(memory, address).map(ClassSelector::Name)
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

fn refuse(state: &ServicesState, teb: u64, error: u32) -> CallbackStart {
    state.set_last_error(teb, error);
    CallbackStart::Return(0)
}

fn register_class(
    windows: &Arc<Mutex<WindowState>>,
    state: &ServicesState,
    teb: u64,
    memory: &GuestMemory,
    address: u64,
) -> Result<u64, ExitReason> {
    let mut bytes = [0; WINDOW_CLASS_SIZE];
    if address == 0 || memory.read_bytes(address, &mut bytes).is_err() {
        return state.fail(teb, ERROR_NOACCESS, 0);
    }
    let entry = read_u64(&bytes, 8);
    if read_u32(&bytes, 0) as usize != WINDOW_CLASS_SIZE
        || entry == 0
        || memory.fetch_instruction_byte(entry).is_err()
    {
        return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
    }
    // The headless model has no extra window/class storage, icons, brush,
    // system cursor or menu. Redraw style bits require an actual surface.
    if read_u32(&bytes, 4) != 0
        || read_u64(&bytes, 16) != 0
        || [32, 40, 48, 56, 72]
            .into_iter()
            .any(|offset| read_u64(&bytes, offset) != 0)
    {
        return state.fail(teb, ERROR_NOT_SUPPORTED, 0);
    }
    let name = match class_name(memory, read_u64(&bytes, 64)) {
        Ok(name) => name,
        Err(error) => return state.fail(teb, error, 0),
    };
    let instance = read_u64(&bytes, 24);
    let mut windows = windows.lock().expect("guest windows");
    if windows
        .classes
        .values()
        .any(|class| class.instance == instance && class.name.eq_ignore_ascii_case(&name))
    {
        return state.fail(teb, ERROR_CLASS_ALREADY_EXISTS, 0);
    }
    let Some(atom) = CLASS_ATOMS
        .clone()
        .find(|atom| !windows.classes.contains_key(atom))
    else {
        return state.fail(teb, ERROR_NOT_ENOUGH_MEMORY, 0);
    };
    windows.classes.insert(
        atom,
        WindowClass {
            name,
            instance,
            entry,
        },
    );
    Ok(u64::from(atom))
}

#[derive(Clone)]
struct Desktop {
    windows: Arc<Mutex<WindowState>>,
    queues: Arc<Mutex<MessageQueues>>,
}

impl Desktop {
    fn reap_exited_windows(&self, state: &ServicesState, memory: &mut GuestMemory) {
        let mut windows = self.windows.lock().expect("guest windows");
        for scratch in windows.retired_scratch.drain(..) {
            memory.release_heap_allocation(PROCESS_HEAP, scratch);
        }
        let mut retired = Vec::new();
        {
            let threads = state.threads.lock().expect("guest thread table");
            let scheduled = state.hooks.is_attached();
            windows.windows.retain(|&hwnd, window| {
                let live = if scheduled {
                    threads.threads.contains_key(&window.owner)
                } else {
                    window.owner == state.thread_id
                };
                if !live {
                    retired.push(hwnd);
                }
                live
            });
        }
        if !retired.is_empty() {
            let mut queues = self.queues.lock().expect("guest message queues");
            for hwnd in retired {
                discard_window_messages(&mut queues, hwnd);
            }
        }
    }

    fn remove_window(&self, hwnd: u64, retired_scratch: Option<u64>) {
        let mut windows = self.windows.lock().expect("guest windows");
        if let Some(scratch) = retired_scratch {
            windows.retired_scratch.push(scratch);
        }
        windows.windows.remove(&hwnd);
        discard_window_messages(&mut self.queues.lock().expect("guest message queues"), hwnd);
    }
}

struct WindowCallback {
    desktop: Desktop,
    hwnd: u64,
    entry: u64,
}

impl WindowCallback {
    fn next(&self, message: u64, wparam: u64, lparam: u64) -> CallbackStep {
        CallbackStep::Call {
            entry: self.entry,
            args: [self.hwnd, message, wparam, lparam],
        }
    }
}

#[derive(Clone, Copy)]
enum CreatePhase {
    Nonclient,
    Client,
    RejectedDestroy,
    RejectedNonclient,
}

struct CreateTask {
    window: WindowCallback,
    scratch: Option<u64>,
    phase: CreatePhase,
    committed: bool,
}

impl Drop for CreateTask {
    fn drop(&mut self) {
        if !self.committed {
            self.window
                .desktop
                .remove_window(self.window.hwnd, self.scratch);
        }
    }
}

impl CallbackTask for CreateTask {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        memory: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        let mut windows = self.window.desktop.windows.lock().expect("guest windows");
        let window = windows.windows.get_mut(&self.window.hwnd);
        if window.is_some() && matches!(self.phase, CreatePhase::Nonclient) && result != 0 {
            self.phase = CreatePhase::Client;
            return Ok(self
                .window
                .next(WM_CREATE, 0, self.scratch.expect("creation scratch")));
        }
        if let Some(scratch) = self.scratch.take() {
            memory.release_heap_allocation(PROCESS_HEAP, scratch);
        }
        let Some(window) = window else {
            return Ok(CallbackStep::Return(0));
        };
        match self.phase {
            CreatePhase::Nonclient => {
                window.destroying = true;
                self.phase = CreatePhase::RejectedNonclient;
                Ok(self.window.next(WM_NCDESTROY, 0, 0))
            }
            CreatePhase::Client if result == u64::MAX => {
                window.destroying = true;
                self.phase = CreatePhase::RejectedDestroy;
                Ok(self.window.next(WM_DESTROY, 0, 0))
            }
            CreatePhase::Client => {
                self.committed = true;
                Ok(CallbackStep::Return(self.window.hwnd))
            }
            CreatePhase::RejectedDestroy => {
                self.phase = CreatePhase::RejectedNonclient;
                Ok(self.window.next(WM_NCDESTROY, 0, 0))
            }
            CreatePhase::RejectedNonclient => Ok(CallbackStep::Return(0)),
        }
    }
}

fn start_creation(
    desktop: &Desktop,
    state: &ServicesState,
    args: &Win64Args,
    teb: u64,
    memory: &mut GuestMemory,
) -> Result<CallbackStart, ExitReason> {
    let mut stack = [0; 8];
    for (index, slot) in stack.iter_mut().enumerate() {
        *slot = args
            .get(index + 4, memory)
            .map_err(ExitReason::MemoryFault)?;
    }
    let [x, y, width, height, parent, menu, instance, params] = stack;
    let style = args.r9 as u32;
    let exstyle = args.rcx as u32;
    if exstyle != 0 || style & !WS_OVERLAPPEDWINDOW != 0 || parent != 0 || menu != 0 {
        return Ok(refuse(state, teb, ERROR_NOT_SUPPORTED));
    }
    let selector = match class_selector(memory, args.rdx) {
        Ok(selector) => selector,
        Err(error) => return Ok(refuse(state, teb, error)),
    };
    let owner = state.thread_id_of(teb);
    desktop.reap_exited_windows(state, memory);
    let mut windows = desktop.windows.lock().expect("guest windows");
    let Some(atom) = windows.class_atom(selector, instance) else {
        return Ok(refuse(state, teb, ERROR_CANNOT_FIND_WND_CLASS));
    };
    if windows.windows.len() >= MAX_WINDOWS {
        return Ok(refuse(state, teb, ERROR_NOT_ENOUGH_MEMORY));
    }
    let Some(hwnd) = windows.last_window.max(FIRST_WINDOW - 1).checked_add(1) else {
        return Ok(refuse(state, teb, ERROR_NOT_ENOUGH_MEMORY));
    };
    let mut bytes = [0; 80];
    for (offset, value) in [
        (0, params),
        (8, instance),
        (16, menu),
        (24, parent),
        (56, args.r8),
        (64, args.rdx),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    for (offset, value) in [
        (32, height as u32),
        (36, width as u32),
        (40, y as u32),
        (44, x as u32),
        (48, style),
        (72, exstyle),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    let scratch = match memory.map_heap_allocation(PROCESS_HEAP, bytes.len()) {
        Ok(scratch) => scratch,
        Err(_) => return Ok(refuse(state, teb, ERROR_NOT_ENOUGH_MEMORY)),
    };
    if let Err(error) = memory.write_bytes(scratch, &bytes) {
        memory.release_heap_allocation(PROCESS_HEAP, scratch);
        return Err(ExitReason::MemoryFault(error));
    }
    let entry = windows.classes[&atom].entry;
    windows.windows.insert(
        hwnd,
        Window {
            atom,
            owner,
            visible: false,
            showing: false,
            destroying: false,
        },
    );
    windows.last_window = hwnd;
    drop(windows);
    let task = CreateTask {
        window: WindowCallback {
            desktop: desktop.clone(),
            hwnd,
            entry,
        },
        scratch: Some(scratch),
        phase: CreatePhase::Nonclient,
        committed: false,
    };
    Ok(CallbackStart::Call {
        task: Box::new(task),
        entry,
        args: [hwnd, WM_NCCREATE, 0, scratch],
    })
}

struct DestroyTask {
    window: WindowCallback,
    after_destroy: bool,
    result: u64,
}

impl Drop for DestroyTask {
    fn drop(&mut self) {
        self.window.desktop.remove_window(self.window.hwnd, None);
    }
}

impl CallbackTask for DestroyTask {
    fn resume(&mut self, _: u64, _: u64, _: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        if self.after_destroy {
            return Ok(CallbackStep::Return(self.result));
        }
        self.after_destroy = true;
        Ok(self.window.next(WM_NCDESTROY, 0, 0))
    }
}

fn start_destruction(
    desktop: &Desktop,
    state: &ServicesState,
    teb: u64,
    hwnd: u64,
    result: u64,
) -> CallbackStart {
    let owner = state.thread_id_of(teb);
    let mut windows = desktop.windows.lock().expect("guest windows");
    let window = match windows.owned_window(hwnd, owner) {
        Ok(window) => window,
        Err(error) => return refuse(state, teb, error),
    };
    if window.destroying {
        return refuse(state, teb, ERROR_INVALID_WINDOW_HANDLE);
    }
    window.destroying = true;
    window.visible = false;
    let atom = window.atom;
    let entry = windows.classes[&atom].entry;
    let task = DestroyTask {
        window: WindowCallback {
            desktop: desktop.clone(),
            hwnd,
            entry,
        },
        after_destroy: false,
        result,
    };
    CallbackStart::Call {
        task: Box::new(task),
        entry,
        args: [hwnd, WM_DESTROY, 0, 0],
    }
}

struct DispatchTask;

impl CallbackTask for DispatchTask {
    fn resume(
        &mut self,
        result: u64,
        _: u64,
        _: &mut GuestMemory,
    ) -> Result<CallbackStep, ExitReason> {
        Ok(CallbackStep::Return(result))
    }
}

struct ShowTask {
    windows: Arc<Mutex<WindowState>>,
    hwnd: u64,
    previous: bool,
    show: bool,
}

impl Drop for ShowTask {
    fn drop(&mut self) {
        let mut windows = self.windows.lock().expect("guest windows");
        if let Some(window) = windows.windows.get_mut(&self.hwnd) {
            window.showing = false;
        }
    }
}

impl CallbackTask for ShowTask {
    fn resume(&mut self, _: u64, _: u64, _: &mut GuestMemory) -> Result<CallbackStep, ExitReason> {
        let mut windows = self.windows.lock().expect("guest windows");
        if let Some(window) = windows.windows.get_mut(&self.hwnd)
            && !window.destroying
        {
            window.visible = self.show;
        }
        Ok(CallbackStep::Return(u64::from(self.previous)))
    }
}

pub(super) fn register(
    registry: &mut ImportRegistry,
    state: &Arc<ServicesState>,
    windows: &Arc<Mutex<WindowState>>,
    queues: &Arc<Mutex<MessageQueues>>,
) {
    let desktop = Desktop {
        windows: Arc::clone(windows),
        queues: Arc::clone(queues),
    };

    let class_windows = Arc::clone(windows);
    let class_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "RegisterClassExA",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            register_class(&class_windows, &class_state, cpu.gs_base, memory, args.rcx)
        },
    );

    let unregister = desktop.clone();
    let unregister_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "UnregisterClassA",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let teb = cpu.gs_base;
            let selector = match class_selector(memory, args.rcx) {
                Ok(selector) => selector,
                Err(error) => return unregister_state.fail(teb, error, 0),
            };
            unregister.reap_exited_windows(&unregister_state, memory);
            let mut windows = unregister.windows.lock().expect("guest windows");
            let Some(atom) = windows.class_atom(selector, args.rdx) else {
                return unregister_state.fail(teb, ERROR_CLASS_DOES_NOT_EXIST, 0);
            };
            if windows.windows.values().any(|window| window.atom == atom) {
                return unregister_state.fail(teb, ERROR_CLASS_HAS_WINDOWS, 0);
            }
            windows.classes.remove(&atom);
            Ok(1)
        },
    );

    let create = desktop.clone();
    let create_state = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "CreateWindowExA",
        ServiceStatus::Partial,
        move |args, teb, memory, _| start_creation(&create, &create_state, args, teb, memory),
    );

    let destroy = desktop.clone();
    let destroy_state = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "DestroyWindow",
        ServiceStatus::Partial,
        move |args, teb, _, _| {
            Ok(start_destruction(
                &destroy,
                &destroy_state,
                teb,
                args.rcx,
                1,
            ))
        },
    );

    let default_state = Arc::clone(state);
    let default_desktop = desktop.clone();
    registry.register_callback_task(
        MODULE,
        "DefWindowProcA",
        ServiceStatus::Partial,
        move |args, teb, _, _| {
            let message = args.rdx as u32;
            if message == WM_CLOSE {
                return Ok(start_destruction(
                    &default_desktop,
                    &default_state,
                    teb,
                    args.rcx,
                    0,
                ));
            }
            let valid = default_desktop
                .windows
                .lock()
                .expect("guest windows")
                .windows
                .contains_key(&args.rcx);
            Ok(CallbackStart::Return(u64::from(
                valid && u64::from(message) == WM_NCCREATE,
            )))
        },
    );

    let show_windows = Arc::clone(windows);
    let show_state = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "ShowWindow",
        ServiceStatus::Partial,
        move |args, teb, _, _| {
            let command = args.rdx as u32;
            let show = match command {
                0 => false,            // SW_HIDE
                1 | 4 | 5 | 8 => true, // SW_SHOWNORMAL, SW_SHOWNOACTIVATE, SW_SHOW, SW_SHOWNA
                _ => return Ok(refuse(&show_state, teb, ERROR_NOT_SUPPORTED)),
            };
            let owner = show_state.thread_id_of(teb);
            let mut windows = show_windows.lock().expect("guest windows");
            let window = match windows.owned_window(args.rcx, owner) {
                Ok(window) if !window.destroying => window,
                Ok(_) => return Ok(refuse(&show_state, teb, ERROR_INVALID_WINDOW_HANDLE)),
                Err(error) => return Ok(refuse(&show_state, teb, error)),
            };
            let previous = window.visible;
            if show == previous {
                return Ok(CallbackStart::Return(u64::from(previous)));
            }
            if window.showing {
                return Ok(refuse(&show_state, teb, ERROR_NOT_SUPPORTED));
            }
            if command == 1 {
                window.visible = show;
                return Ok(CallbackStart::Return(u64::from(previous)));
            }
            // The callback can inspect the previous visibility until the
            // staged change commits. Nested destruction can retire it.
            window.showing = true;
            let atom = window.atom;
            let entry = windows.classes[&atom].entry;
            Ok(CallbackStart::Call {
                task: Box::new(ShowTask {
                    windows: Arc::clone(&show_windows),
                    hwnd: args.rcx,
                    previous,
                    show,
                }),
                entry,
                args: [args.rcx, WM_SHOWWINDOW, u64::from(show), 0],
            })
        },
    );

    let visibility_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "IsWindowVisible",
        ServiceStatus::Partial,
        move |args, _, memory| {
            desktop.reap_exited_windows(&visibility_state, memory);
            let windows = desktop.windows.lock().expect("guest windows");
            Ok(u64::from(
                windows
                    .windows
                    .get(&args.rcx)
                    .is_some_and(|window| window.visible),
            ))
        },
    );
    // The supported show commands cannot minimize windows.
    registry.register_with_status(MODULE, "IsIconic", ServiceStatus::Partial, |_, _, _| Ok(0));

    let dispatch = Arc::clone(windows);
    let dispatch_state = Arc::clone(state);
    registry.register_callback_task(
        MODULE,
        "DispatchMessageA",
        ServiceStatus::Partial,
        move |args, teb, memory, _| {
            let mut message = [0; MSG_SIZE];
            if args.rcx == 0 || memory.read_bytes(args.rcx, &mut message).is_err() {
                return Ok(refuse(&dispatch_state, teb, ERROR_NOACCESS));
            }
            let hwnd = read_u64(&message, 0);
            if hwnd == 0 {
                return Ok(CallbackStart::Return(0));
            }
            let id = read_u32(&message, 8);
            let lparam = read_u64(&message, 24);
            if id == 0x0113 && lparam != 0 {
                return Ok(refuse(&dispatch_state, teb, ERROR_NOT_SUPPORTED));
            }
            let owner = dispatch_state.thread_id_of(teb);
            let mut windows = dispatch.lock().expect("guest windows");
            let atom = match windows.owned_window(hwnd, owner) {
                Ok(window) => window.atom,
                Err(error) => return Ok(refuse(&dispatch_state, teb, error)),
            };
            let entry = windows.classes[&atom].entry;
            Ok(CallbackStart::Call {
                task: Box::new(DispatchTask),
                entry,
                args: [hwnd, u64::from(id), read_u64(&message, 16), lparam],
            })
        },
    );
}
