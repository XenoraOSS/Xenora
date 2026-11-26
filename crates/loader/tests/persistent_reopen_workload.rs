// SPDX-License-Identifier: GPL-3.0-or-later

//! Run the original reopen workload through Win32 services and reference graphics.

use std::sync::{Arc, Mutex};

use xenora_loader::graphics::{FramePresentation, GraphicsBackendId};
use xenora_loader::services::{MockClock, ServicesState, register_standard_services};
use xenora_loader::startup::{StartupLayout, run_synthetic_startup};
use xenora_loader::{ExitReason, ImportRegistry};

const GUEST: &[u8] = include_bytes!("../../../fixtures/pe/persistent-reopen-guest.exe");
const MODULE: &str = "xenora_session.dll";
const JOURNAL: &str = r"C:\xenora-session.log";
const RED: [u8; 4] = [255, 0, 0, 255];
const CLEAR: [u8; 4] = [19, 43, 93, 251];
const LEFT_ROW: [[u8; 4]; 4] = [RED, CLEAR, CLEAR, CLEAR];
const RIGHT_ROW: [[u8; 4]; 4] = [CLEAR, CLEAR, CLEAR, RED];

struct Receipt {
    frame: FramePresentation,
    journal: Option<Vec<u8>>,
    handles: usize,
}

#[derive(Default)]
struct Progress {
    admitted: usize,
    receipts: Vec<Receipt>,
    end_seen: bool,
    finish: Option<(u32, usize)>,
}

fn verify_workload(sequence: &[u8]) {
    let state = Arc::new(ServicesState::new(Arc::new(MockClock::new(0))));
    let presentation = state.graphics.lock().unwrap().last_presentation.clone();
    let mut registry = ImportRegistry::new();
    register_standard_services(&mut registry, Arc::clone(&state));

    let progress = Arc::new(Mutex::new(Progress::default()));
    let next = Arc::clone(&progress);
    let directions = sequence.to_vec();
    registry.register(MODULE, "XenoraNextEvent", move |_, _, _| {
        let mut next = next.lock().map_err(|_| ExitReason::ServiceAbiViolation)?;
        if next.admitted != next.receipts.len() || next.end_seen {
            return Err(ExitReason::ServiceAbiViolation);
        }
        let event = match directions.get(next.admitted) {
            Some(b'L') => 1,
            Some(b'R') => 2,
            Some(_) => return Err(ExitReason::ServiceAbiViolation),
            None => {
                next.end_seen = true;
                return Ok(0);
            }
        };
        next.admitted += 1;
        Ok(event)
    });
    let current = Arc::clone(&progress);
    let frame_state = Arc::clone(&state);
    let last_presentation = Arc::clone(&presentation);
    registry.register(MODULE, "XenoraPresent", move |args, _, _| {
        let mut current = current
            .lock()
            .map_err(|_| ExitReason::ServiceAbiViolation)?;
        let index = current.receipts.len() + 1;
        if current.end_seen || current.admitted != index || args.rcx as u32 as usize != index {
            return Err(ExitReason::ServiceAbiViolation);
        }
        let frame = last_presentation
            .lock()
            .map_err(|_| ExitReason::ServiceAbiViolation)?
            .clone();
        current.receipts.push(Receipt {
            frame,
            journal: frame_state.file_contents(JOURNAL),
            handles: frame_state.open_handle_count(),
        });
        Ok(1)
    });
    let end = Arc::clone(&progress);
    let final_state = Arc::clone(&state);
    registry.register(MODULE, "XenoraFinish", move |args, _, _| {
        let mut end = end.lock().map_err(|_| ExitReason::ServiceAbiViolation)?;
        let count = args.rcx as u32;
        if !end.end_seen || end.finish.is_some() || count as usize != end.receipts.len() {
            return Err(ExitReason::ServiceAbiViolation);
        }
        end.finish = Some((count, final_state.open_handle_count()));
        Ok(1)
    });

    let result = run_synthetic_startup(
        GUEST,
        StartupLayout {
            image_base: 0x1_4000_0000,
            stack_top: 0x2_0001_0000,
            tls_base: 0x3_0000_0000,
        },
        registry,
        100_000,
    )
    .expect("start the original reopen workload");
    assert_eq!(result.execution.exit_reason, ExitReason::ProcessExit(0));
    assert!(result.dbt_metrics.dbt_dispatches > 0);
    assert_eq!(state.exit_status(), Some(0));

    let progress = progress.lock().unwrap();
    assert!(progress.end_seen);
    assert_eq!(progress.finish, Some((sequence.len() as u32, 0)));
    assert_eq!(progress.receipts.len(), sequence.len());
    for (index, receipt) in progress.receipts.iter().enumerate() {
        assert_eq!(
            receipt.journal.as_deref(),
            Some(&sequence[..=index]),
            "journal prefix at frame {}",
            index + 1
        );
        assert_eq!(
            receipt.handles, 1,
            "only the retained event may remain open"
        );
        assert_eq!(receipt.frame.frame_index, (index + 1) as u64);
        assert_eq!(receipt.frame.draw_count, 1);
        assert_eq!((receipt.frame.width, receipt.frame.height), (4, 3));
        assert_eq!(
            receipt.frame.backend,
            Some(GraphicsBackendId::SoftwareReference)
        );
        let row = if sequence[index] == b'L' {
            LEFT_ROW
        } else {
            RIGHT_ROW
        };
        assert_eq!(
            receipt.frame.pixels,
            row.concat().repeat(3),
            "RGBA at frame {}",
            index + 1
        );
    }

    assert_eq!(
        presentation.lock().unwrap().frame_index as usize,
        sequence.len()
    );
    assert_eq!(state.file_contents(JOURNAL).as_deref(), Some(sequence));
    assert_eq!(state.open_handle_count(), 0);
}

#[test]
fn persistent_reopen_workload_checks_every_prefix_and_frame() {
    const FULL: &[u8; 32] = b"LLRRLLRRLLRRLLRRLLRRLLRRLLRRLLRR";
    verify_workload(FULL);
    // Reusing the driver starts a fresh process and synthetic drive.
    verify_workload(b"RLLRR");
}

#[test]
fn persistent_reopen_workload_finishes_with_an_empty_journal_and_no_handles() {
    verify_workload(b"");
}
