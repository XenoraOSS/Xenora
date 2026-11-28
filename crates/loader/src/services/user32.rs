// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic USER32 windows and posted messages within a guest process.

use super::{ERROR_INVALID_PARAMETER, ERROR_NOACCESS, ERROR_NOT_SUPPORTED, ServicesState};
use crate::cpu::ExitReason;
use crate::imports::{ImportRegistry, ServiceStatus};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

mod windows;

const MODULE: &str = "user32.dll";
const ERROR_INVALID_WINDOW_HANDLE: u32 = 1400;
const ERROR_INVALID_THREAD_ID: u32 = 1444;
const ERROR_NOT_ENOUGH_QUOTA: u32 = 1816;
const MAX_POSTED_MESSAGES: usize = 10_000;
const MSG_SIZE: usize = 48;
const WM_CLOSE: u32 = 0x0010;
const WM_USER: u32 = 0x0400;
const WM_APP_LAST: u32 = 0xbfff;
const WM_QUIT: u32 = 0x0012;
const PM_REMOVE: u32 = 0x0001;
const PM_NOYIELD: u32 = 0x0002;

struct PostedMessage {
    hwnd: u64,
    message: u32,
    wparam: u64,
    lparam: u64,
    time: u32,
}

impl PostedMessage {
    fn encode(&self) -> [u8; MSG_SIZE] {
        // The x64 MSG has an eight-byte HWND, alignment padding after the
        // message, two pointer-sized parameters, a timestamp and a POINT.
        // Thread messages have a zero HWND. There is no host cursor.
        let mut bytes = [0; MSG_SIZE];
        bytes[..8].copy_from_slice(&self.hwnd.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.message.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.wparam.to_le_bytes());
        bytes[24..32].copy_from_slice(&self.lparam.to_le_bytes());
        bytes[32..36].copy_from_slice(&self.time.to_le_bytes());
        bytes
    }

    fn matches(&self, window_filter: u64, minimum: u32, maximum: u32) -> bool {
        let matches_window = match window_filter {
            0 => true,
            u64::MAX => self.hwnd == 0,
            hwnd => self.hwnd == hwnd,
        };
        matches_window
            && (self.message == WM_QUIT
                || (minimum == 0 && maximum == 0)
                || (minimum..=maximum).contains(&self.message))
    }
}

type MessageQueues = BTreeMap<u32, VecDeque<PostedMessage>>;

fn discard_window_messages(queues: &mut MessageQueues, hwnd: u64) {
    for queue in queues.values_mut() {
        queue.retain(|message| message.hwnd != hwnd);
    }
}

fn prune_message_queues(state: &ServicesState, queues: &mut MessageQueues) {
    let threads = state.threads.lock().expect("guest thread table");
    if state.hooks.is_attached() {
        queues.retain(|id, _| threads.threads.contains_key(id));
    } else {
        queues.retain(|id, _| *id == state.thread_id);
    }
}

fn register_message_queues(
    registry: &mut ImportRegistry,
    state: &Arc<ServicesState>,
    windows: &Arc<Mutex<windows::WindowState>>,
    queues: &Arc<Mutex<MessageQueues>>,
) {
    for symbol in ["PeekMessageA", "PeekMessageW"] {
        let windows = Arc::clone(windows);
        let queues = Arc::clone(queues);
        let state = Arc::clone(state);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, memory| {
                let teb = cpu.gs_base;
                let id = state.thread_id_of(teb);
                if !matches!(args.rdx, 0 | u64::MAX)
                    && windows
                        .lock()
                        .expect("guest window state")
                        .owner_of(args.rdx)
                        != Some(id)
                {
                    return Ok(0);
                }
                let minimum = args.r8 as u32;
                let maximum = args.r9 as u32;
                let flags = args.get(4, memory).map_err(ExitReason::MemoryFault)? as u32;
                if minimum > u16::MAX as u32
                    || maximum > u16::MAX as u32
                    || flags & !(PM_REMOVE | PM_NOYIELD) != 0
                {
                    return state.fail(teb, ERROR_INVALID_PARAMETER, 0);
                }

                let mut queues = queues.lock().expect("guest message queues");
                prune_message_queues(&state, &mut queues);
                let queue = queues.entry(id).or_default();
                let Some(index) = queue
                    .iter()
                    .position(|message| message.matches(args.rdx, minimum, maximum))
                else {
                    return Ok(0);
                };
                let output = args.rcx;
                if output == 0
                    || !memory.can_write(output, MSG_SIZE)
                    || memory.write_bytes(output, &queue[index].encode()).is_err()
                {
                    return state.fail(teb, ERROR_NOACCESS, 0);
                }
                if flags & PM_REMOVE != 0 {
                    queue.remove(index);
                }
                Ok(1)
            },
        );
    }

    for symbol in ["PostThreadMessageA", "PostThreadMessageW"] {
        let queues = Arc::clone(queues);
        let state = Arc::clone(state);
        registry.register_with_status(
            MODULE,
            symbol,
            ServiceStatus::Partial,
            move |args, cpu, _| {
                let mut queues = queues.lock().expect("guest message queues");
                prune_message_queues(&state, &mut queues);
                let Some(queue) = queues.get_mut(&(args.rcx as u32)) else {
                    return state.fail(cpu.gs_base, ERROR_INVALID_THREAD_ID, 0);
                };
                if queue.len() >= MAX_POSTED_MESSAGES {
                    return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_QUOTA, 0);
                }
                queue.push_back(PostedMessage {
                    hwnd: 0,
                    message: args.rdx as u32,
                    wparam: args.r8,
                    lparam: args.r9,
                    time: state.uptime_millis() as u32,
                });
                Ok(1)
            },
        );
    }

    let windows = Arc::clone(windows);
    let queues = Arc::clone(queues);
    let state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "PostMessageA",
        ServiceStatus::Partial,
        move |args, cpu, _| {
            // A flat synthetic window can receive WM_CLOSE and application-
            // defined messages. There is no broadcasting or host input.
            let message = args.rdx as u32;
            if matches!(args.rcx, 0 | 0xffff) {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            if message != WM_CLOSE && !(WM_USER..=WM_APP_LAST).contains(&message) {
                return state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            // Keep ownership validation and insertion ordered with destruction.
            let windows = windows.lock().expect("guest window state");
            let Some(owner) = windows.owner_of(args.rcx) else {
                return state.fail(cpu.gs_base, ERROR_INVALID_WINDOW_HANDLE, 0);
            };
            let mut queues = queues.lock().expect("guest message queues");
            prune_message_queues(&state, &mut queues);
            let owner_alive = if owner == state.thread_id {
                state.main_thread_alive()
            } else {
                state.worker_object(owner).is_some()
            };
            if !owner_alive {
                return state.fail(cpu.gs_base, ERROR_INVALID_WINDOW_HANDLE, 0);
            }
            let queue = queues.entry(owner).or_default();
            if queue.len() >= MAX_POSTED_MESSAGES {
                return state.fail(cpu.gs_base, ERROR_NOT_ENOUGH_QUOTA, 0);
            }
            queue.push_back(PostedMessage {
                hwnd: args.rcx,
                message,
                wparam: args.r8,
                lparam: args.r9,
                time: state.uptime_millis() as u32,
            });
            Ok(1)
        },
    );
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let windows = Arc::new(Mutex::new(windows::WindowState::default()));
    let queues = Arc::new(Mutex::new(MessageQueues::new()));
    register_message_queues(registry, state, &windows, &queues);
    windows::register(registry, state, &windows, &queues);

    // Ordinary messages do not produce characters. The headless model has
    // no keyboard layout with which to translate virtual-key messages.
    let translate_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "TranslateMessage",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            let mut message = [0; MSG_SIZE];
            if args.rcx == 0 || memory.read_bytes(args.rcx, &mut message).is_err() {
                return translate_state.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            let id = u32::from_le_bytes(message[8..12].try_into().unwrap());
            if matches!(id, 0x0100 | 0x0101 | 0x0104 | 0x0105) {
                return translate_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, 0);
            }
            Ok(0)
        },
    );

    // This subset of MsgWaitForMultipleObjects observes kernel objects only.
    // It does not wake for posted thread messages; guests poll them with PeekMessage.
    let wait_state = Arc::clone(state);
    registry.register_with_status(
        MODULE,
        "MsgWaitForMultipleObjects",
        ServiceStatus::Partial,
        move |args, cpu, memory| {
            use super::events::{WAIT_FAILED, resume_wait, wait_for_objects};
            use super::{ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED};
            use crate::cpu::ExitReason;
            if let Some(result) = resume_wait(&wait_state, cpu.gs_base) {
                return result;
            }
            let count = args.rcx as u32;
            let address = args.rdx;
            let timeout = args.r8 as u32;
            let _wake_mask = args.r9 as u32;
            let flags = args.get(4, memory).map_err(ExitReason::MemoryFault)? as u32;
            const MWMO_WAITALL: u32 = 1;
            const MWMO_ALERTABLE: u32 = 2;
            const MWMO_INPUTAVAILABLE: u32 = 4;
            if flags & !(MWMO_WAITALL | MWMO_ALERTABLE | MWMO_INPUTAVAILABLE) != 0 {
                return wait_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, WAIT_FAILED);
            }
            if flags & MWMO_ALERTABLE != 0 {
                return wait_state.fail(cpu.gs_base, ERROR_NOT_SUPPORTED, WAIT_FAILED);
            }
            if !(1..=64).contains(&count) || address == 0 {
                return wait_state.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, WAIT_FAILED);
            }
            let mut handles = [0; 64];
            let handles = &mut handles[..count as usize];
            for (index, handle) in handles.iter_mut().enumerate() {
                let slot = address
                    .checked_add(index as u64 * 8)
                    .ok_or(ExitReason::MemoryFault(
                        crate::memory::MemoryError::Overflow,
                    ))?;
                *handle = memory.read_u64(slot).map_err(ExitReason::MemoryFault)?;
            }
            wait_for_objects(
                &wait_state,
                cpu.gs_base,
                handles,
                flags & MWMO_WAITALL != 0,
                timeout,
            )
        },
    );

    for (module, symbols) in [
        (
            "ext-ms-win-rtcore-ntuser-window-ansi-l1-1-0.dll",
            &["RegisterClassExA", "CreateWindowExA", "DefWindowProcA"][..],
        ),
        (
            "ext-ms-win-rtcore-ntuser-window-l1-1-0.dll",
            &["ShowWindow"],
        ),
        (
            "ext-ms-win-rtcore-ntuser-message-ansi-l1-1-0.dll",
            &["DispatchMessageA", "PeekMessageA"],
        ),
        (
            "ext-ms-win-rtcore-ntuser-message-l1-1-0.dll",
            &["TranslateMessage"],
        ),
    ] {
        for symbol in symbols {
            registry.alias(module, MODULE, symbol);
        }
    }
}

#[cfg(test)]
#[test]
fn a_new_thread_message_queue_is_empty() {
    use crate::cpu::{CpuState, Register};
    use crate::import_table::ImportName;
    use crate::imports::Win64Args;
    use crate::memory::{GuestMemory, MemoryPermissions};
    use crate::services::MockClock;

    let mut registry = ImportRegistry::new();
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    register(&mut registry, &state);
    let mut memory = GuestMemory::new();
    memory
        .map_region(0x1000, 8192, MemoryPermissions::READ_WRITE)
        .unwrap();
    for (symbol, expected) in [
        ("PeekMessageA", 0),
        ("TranslateMessage", 0),
        ("IsWindowVisible", 0),
        ("IsIconic", 0),
    ] {
        let mut cpu = CpuState::default();
        if matches!(symbol, "PeekMessageA" | "TranslateMessage") {
            cpu.set(Register::Rcx, 0x1300);
        }
        if symbol == "PeekMessageA" {
            cpu.set(Register::Rsp, 0x2000);
            memory.write_u64(0x2028, 0).unwrap();
        }
        let descriptor = registry
            .resolve(MODULE, &ImportName::Name(symbol.to_owned()))
            .unwrap();
        assert_eq!(
            (descriptor.handler)(&Win64Args::new(&cpu), &mut cpu, &mut memory).unwrap(),
            expected,
            "{symbol} answers honestly for a windowless machine"
        );
    }

    // MsgWaitForMultipleObjects: invalid counts and handles fail honestly,
    // and the wait machinery serves the object half of the contract.
    {
        let wait = registry
            .resolve(
                MODULE,
                &ImportName::Name("MsgWaitForMultipleObjects".to_owned()),
            )
            .unwrap();
        let call_wait = |memory: &mut GuestMemory, args: &[u64]| {
            let mut cpu = CpuState::default();
            for (reg, &value) in [Register::Rcx, Register::Rdx, Register::R8, Register::R9]
                .into_iter()
                .zip(args)
            {
                cpu.set(reg, value);
            }
            cpu.set(Register::Rsp, 0x2000);
            for (index, &value) in args.iter().enumerate().skip(4) {
                memory
                    .write_u64(0x2000 + 0x28 + (index as u64 - 4) * 8, value)
                    .unwrap();
            }
            (wait.handler)(&Win64Args::new(&cpu), &mut cpu, memory).unwrap()
        };
        memory.write_u64(0x1300, 0xDEAD).unwrap();
        // A count of zero or beyond 64 is invalid, whatever the handles say.
        assert_eq!(
            call_wait(&mut memory, &[0, 0x1300, 0, 0, 0]),
            u32::MAX as u64
        );
        assert_eq!(
            call_wait(&mut memory, &[65, 0x1300, 0, 0, 0]),
            u32::MAX as u64
        );
        // A handle the kernel does not know is WAIT_FAILED.
        assert_eq!(
            call_wait(&mut memory, &[1, 0x1300, 0, 0, 0]),
            u32::MAX as u64
        );
        // Unknown flags are invalid; the alertable form is honestly refused.
        assert_eq!(
            call_wait(&mut memory, &[1, 0x1300, 0, 0, 8]),
            u32::MAX as u64
        );
        assert_eq!(
            call_wait(&mut memory, &[1, 0x1300, 0, 0, 2]),
            u32::MAX as u64
        );
    }
}

#[cfg(test)]
#[test]
fn synthetic_window_filters_and_destruction_preserve_thread_messages() {
    let posted = |hwnd, message| PostedMessage {
        hwnd,
        message,
        wparam: 0,
        lparam: 0,
        time: 0,
    };
    let window = 0x1234;
    let record = posted(window, WM_USER);
    assert!(record.matches(0, WM_USER, WM_USER));
    assert!(record.matches(window, WM_USER, WM_USER));
    assert!(!record.matches(u64::MAX, 0, 0));
    assert!(!record.matches(window + 1, 0, 0));
    assert!(!record.matches(window, WM_CLOSE, WM_CLOSE));

    let quit = posted(0, WM_QUIT);
    assert!(quit.matches(0, WM_USER, WM_USER));
    assert!(quit.matches(u64::MAX, WM_USER, WM_USER));
    assert!(!quit.matches(window, 0, 0));

    let mut queues = MessageQueues::from([(1, VecDeque::from([record, quit]))]);
    discard_window_messages(&mut queues, window);
    assert_eq!(queues[&1].len(), 1);
    assert_eq!(queues[&1][0].message, WM_QUIT);
}
