// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use std::sync::mpsc::TryRecvError;
use xenora_loader::import_table::ImportName;
use xenora_loader::{CpuState, GuestMemory, Win64Args};

const ABI_ERROR: ExitReason = ExitReason::ServiceAbiViolation;
const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [19, 43, 93, 251];

struct Exchange {
    registry: ImportRegistry,
    commands: Option<Sender<Direction>>,
    reports: Receiver<Report>,
    presentation: Arc<Mutex<FramePresentation>>,
    progress: Arc<Mutex<GuestProgress>>,
    services: Arc<ServicesState>,
}

impl Exchange {
    fn new() -> Self {
        let mut registry = ImportRegistry::new();
        let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
        register_standard_services(&mut registry, Arc::clone(&services));
        let (commands, pending) = mpsc::channel();
        let (report, reports) = mpsc::channel();
        let presentation = Arc::new(Mutex::new(FramePresentation::default()));
        let progress = install_exchange(
            &mut registry,
            pending,
            report,
            Arc::clone(&presentation),
            Arc::new(AtomicUsize::new(0)),
            Backend::Reference,
            Arc::clone(&services),
        );
        Self {
            registry,
            commands: Some(commands),
            reports,
            presentation,
            progress,
            services,
        }
    }

    fn call(&self, module: &str, symbol: &str, rcx: u64) -> Result<u64, ExitReason> {
        let descriptor = self
            .registry
            .resolve(module, &ImportName::Name(symbol.to_owned()))
            .unwrap_or_else(|| panic!("{module}!{symbol} was not registered"));
        let args = Win64Args {
            rcx,
            rdx: 0,
            r8: 0,
            r9: 0,
            rsp: 0x8000,
        };
        (descriptor.handler)(
            &args,
            &mut CpuState::new(0x1000, args.rsp),
            &mut GuestMemory::new(),
        )
    }

    fn session_call(&self, symbol: &str, value: u64) -> Result<u64, ExitReason> {
        self.call(MODULE, symbol, value)
    }

    fn send(&self, direction: Direction) {
        self.commands.as_ref().unwrap().send(direction).unwrap();
    }

    fn set_frame(&self, sequence: u64, pixels: Vec<u8>) {
        *self.presentation.lock().unwrap() = FramePresentation {
            width: 4,
            height: 3,
            pixels,
            frame_index: sequence,
            draw_count: 1,
            backend: Some(GraphicsBackendId::SoftwareReference),
        };
    }

    fn assert_no_report(&self) {
        assert!(matches!(self.reports.try_recv(), Err(TryRecvError::Empty)));
    }

    fn assert_frame_report(&self, sequence: u32, pixels: &[u8]) {
        let Ok(Report::Frame(frame)) = self.reports.try_recv() else {
            panic!("expected a frame report");
        };
        assert_eq!(frame.sequence, sequence);
        assert_eq!(frame.rgba, pixels);
    }
}

fn pixels(row: [[u8; 4]; 4]) -> Vec<u8> {
    [row; 3].into_iter().flatten().flatten().collect()
}

#[test]
fn rejects_wrong_direction_and_corrupt_channels_before_publishing() {
    let guest = Exchange::new();
    guest.send(Direction::Left);
    assert_eq!(guest.session_call("XenoraNextEvent", 0), Ok(1));

    let left = pixels([RED, CLEAR, CLEAR, CLEAR]);
    let right = pixels([CLEAR, CLEAR, CLEAR, RED]);
    guest.set_frame(1, right.clone());
    assert_eq!(guest.session_call("XenoraPresent", 1), Err(ABI_ERROR));
    guest.assert_no_report();

    // Corrupt one channel of a clear pixel in the last row on each attempt.
    for channel in 0..4 {
        let mut corrupt = left.clone();
        corrupt[36 + channel] ^= 1;
        guest.set_frame(1, corrupt);
        assert_eq!(guest.session_call("XenoraPresent", 1), Err(ABI_ERROR));
        guest.assert_no_report();
    }

    // Rejecting a presentation keeps the first event pending for a retry.
    guest.set_frame(1, left.clone());
    assert_eq!(guest.session_call("XenoraPresent", 1), Ok(1));
    guest.assert_frame_report(1, &left);
    guest.send(Direction::Right);
    assert_eq!(guest.session_call("XenoraNextEvent", 0), Ok(2));
    guest.set_frame(2, right.clone());
    assert_eq!(guest.session_call("XenoraPresent", 2), Ok(1));
    guest.assert_frame_report(2, &right);
    guest.assert_no_report();
}

#[test]
fn final_confirmation_requires_eof_an_exact_count_and_no_live_handles() {
    let mut guest = Exchange::new();
    assert_eq!(guest.session_call("XenoraFinish", 0), Err(ABI_ERROR));
    guest.send(Direction::Right);
    assert_eq!(guest.session_call("XenoraNextEvent", 0), Ok(2));
    assert_eq!(guest.session_call("XenoraFinish", 0), Err(ABI_ERROR));
    let right = pixels([CLEAR, CLEAR, CLEAR, RED]);
    guest.set_frame(1, right.clone());
    assert_eq!(guest.session_call("XenoraPresent", 1), Ok(1));
    guest.assert_frame_report(1, &right);
    assert_eq!(guest.session_call("XenoraFinish", 1), Err(ABI_ERROR));

    guest.commands.take();
    assert_eq!(guest.session_call("XenoraNextEvent", 0), Ok(0));
    assert!(!guest.progress.lock().unwrap().finished);
    assert_eq!(guest.session_call("XenoraFinish", 0), Err(ABI_ERROR));
    assert_eq!(guest.session_call("XenoraFinish", 2), Err(ABI_ERROR));

    let handle = guest.call("kernel32.dll", "CreateEventW", 0).unwrap();
    assert_ne!(handle, 0);
    assert_eq!(guest.services.open_handle_count(), 1);
    assert_eq!(guest.session_call("XenoraFinish", 1), Err(ABI_ERROR));
    assert!(!guest.progress.lock().unwrap().finished);

    assert_eq!(guest.call("kernel32.dll", "CloseHandle", handle), Ok(1));
    assert_eq!(guest.services.open_handle_count(), 0);
    assert_eq!(guest.session_call("XenoraFinish", 1), Ok(1));
    assert!(guest.progress.lock().unwrap().finished);
    assert_eq!(guest.session_call("XenoraFinish", 1), Err(ABI_ERROR));
    guest.assert_no_report();
}
