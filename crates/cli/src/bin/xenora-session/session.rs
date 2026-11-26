// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use xenora_loader::graphics::{FramePresentation, GraphicsBackendId, GraphicsError};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{ExitReason, ImportRegistry};
use xenora_metal::NativeCompute;
use xenora_shader::{Architecture, PIXEL_ENTRY_POINT, VERTEX_ENTRY_POINT, compile_graphics};

#[cfg(test)]
mod tests;

const GUEST: &[u8] = include_bytes!("../../../../../fixtures/pe/persistent-guest.exe");
const VERTEX_SHADER: &[u8] = include_bytes!("../../../../../fixtures/rdna2/x9-constant-vs.bin");
const PIXEL_SHADER: &[u8] = include_bytes!("../../../../../fixtures/rdna2/x9-constant-ps.bin");
const JOURNAL: &str = "C:\\xenora-session.log";
const MODULE: &str = "xenora_session.dll";
pub(super) const WIDTH: usize = 4;
pub(super) const HEIGHT: usize = 3;
pub(super) const MAX_EVENTS: usize = 32;
pub(super) const MAX_STEPS: usize = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Backend {
    Metal,
    Reference,
}

impl Backend {
    fn id(self) -> GraphicsBackendId {
        match self {
            Self::Metal => GraphicsBackendId::MetalNative,
            Self::Reference => GraphicsBackendId::SoftwareReference,
        }
    }

    fn expected_native_draws(self, frames: usize) -> usize {
        match self {
            Self::Metal => frames,
            Self::Reference => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Direction {
    Left,
    Right,
}

impl Direction {
    fn guest_code(self) -> u64 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
        }
    }

    fn journal_byte(self) -> u8 {
        match self {
            Self::Left => b'L',
            Self::Right => b'R',
        }
    }
}

pub(super) struct Frame {
    pub(super) sequence: u32,
    pub(super) rgba: Vec<u8>,
}

pub(super) struct CompletedSession {
    pub(super) frames: Vec<Frame>,
    pub(super) journal: Vec<u8>,
    pub(super) exit_status: u32,
    pub(super) open_handles: usize,
    pub(super) native_draws: usize,
}

struct Finished {
    journal: Vec<u8>,
    exit_status: u32,
    open_handles: usize,
    native_draws: usize,
}

enum Report {
    Frame(Frame),
    Finished(Result<Finished, String>),
}

/// One running PE and its native DBT stay on the worker until the host closes
/// the command channel. Each host request can publish exactly one frame.
struct RunningSession {
    commands: Option<SyncSender<Direction>>,
    reports: Receiver<Report>,
    worker: Option<JoinHandle<()>>,
}

impl RunningSession {
    fn start(backend: Backend, steps: usize) -> Result<Self, String> {
        let (commands, pending) = mpsc::sync_channel(1);
        let (report, reports) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("xenora-guest-application".to_owned())
            .spawn(move || {
                let outcome = execute_guest(backend, steps, pending, report.clone());
                let _ = report.send(Report::Finished(outcome));
            })
            .map_err(|error| format!("start guest worker: {error}"))?;
        Ok(Self {
            commands: Some(commands),
            reports,
            worker: Some(worker),
        })
    }

    fn advance(&mut self, direction: Direction) -> Result<Frame, String> {
        let commands = self
            .commands
            .as_ref()
            .ok_or("guest command stream is closed")?;
        // Even if startup already failed and closed the receiver, the worker
        // can still have a precise failure waiting in its report stream.
        let _ = commands.send(direction);
        match self.receive()? {
            Report::Frame(frame) => Ok(frame),
            Report::Finished(result) => match result {
                Ok(_) => Err("guest exited before presenting the requested frame".to_owned()),
                Err(error) => Err(error),
            },
        }
    }

    fn finish(mut self) -> Result<Finished, String> {
        self.commands.take();
        match self.receive()? {
            Report::Finished(result) => result,
            Report::Frame(_) => {
                Err("guest presented an unrequested frame during shutdown".to_owned())
            }
        }
    }

    fn receive(&self) -> Result<Report, String> {
        self.reports
            .recv()
            .map_err(|_| "guest worker disconnected without a terminal report".to_owned())
    }
}

impl Drop for RunningSession {
    fn drop(&mut self) {
        // Closing this channel wakes a guest waiting for another event even
        // when the host abandons a session after a failed report.
        self.commands.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(super) fn run(
    backend: Backend,
    steps: usize,
    directions: &[Direction],
) -> Result<CompletedSession, String> {
    if !(1..=MAX_STEPS).contains(&steps) || directions.len() > MAX_EVENTS {
        return Err("session exceeds its instruction or input limits".to_owned());
    }
    let mut session = RunningSession::start(backend, steps)?;
    let frames = directions
        .iter()
        .map(|&direction| session.advance(direction))
        .collect::<Result<Vec<_>, _>>()?;
    let finished = session.finish()?;
    let journal: Vec<_> = directions
        .iter()
        .map(|direction| direction.journal_byte())
        .collect();
    if finished.journal != journal {
        return Err("guest journal does not match the delivered events".to_owned());
    }
    Ok(CompletedSession {
        frames,
        journal: finished.journal,
        exit_status: finished.exit_status,
        open_handles: finished.open_handles,
        native_draws: finished.native_draws,
    })
}

#[derive(Default)]
struct GuestProgress {
    presented: usize,
    pending: Option<Direction>,
    stopping: bool,
    finished: bool,
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, ExitReason> {
    mutex.lock().map_err(|_| ExitReason::ServiceAbiViolation)
}

fn install_exchange(
    registry: &mut ImportRegistry,
    commands: Receiver<Direction>,
    report: Sender<Report>,
    presentation: Arc<Mutex<FramePresentation>>,
    native_draws: Arc<AtomicUsize>,
    backend: Backend,
    services: Arc<ServicesState>,
) -> Arc<Mutex<GuestProgress>> {
    let progress = Arc::new(Mutex::new(GuestProgress::default()));
    let next = Arc::clone(&progress);
    let commands = Mutex::new(commands);
    registry.register(MODULE, "XenoraNextEvent", move |_, _, _| {
        {
            let state = lock(&next)?;
            if state.pending.is_some() || state.stopping {
                return Err(ExitReason::ServiceAbiViolation);
            }
        }
        let event = lock(&commands)?.recv();
        let mut state = lock(&next)?;
        match event {
            Ok(direction) if state.presented < MAX_EVENTS => {
                state.pending = Some(direction);
                Ok(direction.guest_code())
            }
            Ok(_) => Err(ExitReason::ServiceAbiViolation),
            Err(_) => {
                state.stopping = true;
                Ok(0)
            }
        }
    });
    let current = Arc::clone(&progress);
    registry.register(MODULE, "XenoraPresent", move |args, _, _| {
        let mut state = lock(&current)?;
        let direction = state.pending.ok_or(ExitReason::ServiceAbiViolation)?;
        let next = state.presented + 1;
        let frame = lock(&presentation)?;
        if args.rcx as u32 as usize != next
            || frame.frame_index != next as u64
            || frame.backend != Some(backend.id())
            || frame.width != WIDTH
            || frame.height != HEIGHT
            || frame.draw_count != 1
            || !matches_frame_pixels(direction, &frame.pixels)
            || native_draws.load(Ordering::Relaxed) != backend.expected_native_draws(next)
        {
            return Err(ExitReason::ServiceAbiViolation);
        }
        let rendered = Frame {
            sequence: next as u32,
            rgba: frame.pixels.clone(),
        };
        report
            .send(Report::Frame(rendered))
            .map_err(|_| ExitReason::ServiceAbiViolation)?;
        state.presented = next;
        state.pending = None;
        Ok(1)
    });
    let final_progress = Arc::clone(&progress);
    registry.register(MODULE, "XenoraFinish", move |args, _, _| {
        let mut state = lock(&final_progress)?;
        if !state.stopping
            || state.pending.is_some()
            || state.finished
            || args.rcx as u32 as usize != state.presented
            || services.open_handle_count() != 0
        {
            return Err(ExitReason::ServiceAbiViolation);
        }
        state.finished = true;
        Ok(1)
    });
    progress
}

fn matches_frame_pixels(direction: Direction, rgba: &[u8]) -> bool {
    const RED: [u8; 4] = [255, 0, 0, 255];
    const CLEAR: [u8; 4] = [19, 43, 93, 251];
    let red_column = match direction {
        Direction::Left => 0,
        Direction::Right => WIDTH - 1,
    };
    let (pixels, _) = rgba.as_chunks::<4>();
    rgba.len() == WIDTH * HEIGHT * 4
        && pixels.iter().enumerate().all(|(index, &pixel)| {
            let expected = if index % WIDTH == red_column {
                RED
            } else {
                CLEAR
            };
            pixel == expected
        })
}

fn execute_guest(
    backend: Backend,
    steps: usize,
    commands: Receiver<Direction>,
    report: Sender<Report>,
) -> Result<Finished, String> {
    let services = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let native_draws = Arc::new(AtomicUsize::new(0));
    if backend == Backend::Metal {
        install_metal(&services, Arc::clone(&native_draws))?;
    }
    let presentation = services
        .graphics
        .lock()
        .map_err(|_| "guest graphics state is poisoned")?
        .last_presentation
        .clone();
    let mut imports = ImportRegistry::new();
    register_standard_services(&mut imports, Arc::clone(&services));
    let progress = install_exchange(
        &mut imports,
        commands,
        report,
        Arc::clone(&presentation),
        Arc::clone(&native_draws),
        backend,
        Arc::clone(&services),
    );
    let result = run_synthetic_startup(
        GUEST,
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        imports,
        steps,
    )
    .map_err(|error| format!("guest startup: {error}"))?;
    let exit_status = match result.execution.exit_reason {
        ExitReason::ProcessExit(0) => 0,
        ExitReason::ProcessExit(status) => {
            return Err(format!("guest application exited with status {status}"));
        }
        ExitReason::StepLimitExceeded => {
            return Err(format!(
                "guest instruction budget exhausted ({steps} steps)"
            ));
        }
        reason => return Err(format!("guest execution stopped: {reason:?}")),
    };
    let state = progress
        .lock()
        .map_err(|_| "guest protocol state is poisoned")?;
    if state.pending.is_some()
        || !state.stopping
        || !state.finished
        || services.exit_status() != Some(exit_status)
    {
        return Err("guest exited without completing its application protocol".to_owned());
    }
    let frame_index = presentation
        .lock()
        .map_err(|_| "guest graphics presentation is poisoned")?
        .frame_index;
    if frame_index != state.presented as u64 {
        return Err("guest drew a frame without presenting it".to_owned());
    }
    let open_handles = services.open_handle_count();
    if open_handles != 0 {
        return Err(format!(
            "guest retained {open_handles} open runtime handles"
        ));
    }
    let journal = services
        .file_contents(JOURNAL)
        .ok_or("guest did not create its journal")?;
    Ok(Finished {
        journal,
        exit_status,
        open_handles,
        native_draws: native_draws.load(Ordering::Relaxed),
    })
}

fn install_metal(services: &ServicesState, native_draws: Arc<AtomicUsize>) -> Result<(), String> {
    let gpu = NativeCompute::new().map_err(|error| format!("native Metal device: {error}"))?;
    let msl = compile_graphics(Architecture::Gfx1030, VERTEX_SHADER, PIXEL_SHADER)
        .map_err(|error| format!("translate application graphics: {error}"))?
        .msl;
    services
        .graphics
        .lock()
        .map_err(|_| "guest graphics state is poisoned")?
        .set_metal_backend(move |vertices, (width, height), clear| {
            let pixels = gpu
                .render_rgba8(
                    &msl,
                    [VERTEX_ENTRY_POINT, PIXEL_ENTRY_POINT],
                    vertices,
                    &[[0.0; 4]; 3],
                    [width, height],
                    clear,
                )
                .map_err(|error| GraphicsError::BackendError(format!("Metal draw: {error}")))?;
            native_draws.fetch_add(1, Ordering::Relaxed);
            Ok(pixels)
        });
    Ok(())
}
