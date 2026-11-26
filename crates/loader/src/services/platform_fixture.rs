// SPDX-License-Identifier: GPL-3.0-or-later

//! Process-local platform services for original guest fixtures.

use super::{ERROR_INVALID_PARAMETER, ERROR_NOACCESS, ERROR_NOT_ENOUGH_MEMORY, ServicesState};
use crate::imports::ImportRegistry;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

const MODULE: &str = "xenora_fixture.dll";
const FIXTURE_APP_ID: u64 = 0x5845_4e4f_5241_0001;
const ERROR_NOT_FOUND: u32 = 1168;
const ERROR_INVALID_STATE: u32 = 5023;
const MAX_SETTINGS: usize = 32;
const MAX_SAVE_SLOTS: usize = 32;
const MAX_SAVE_BYTES: usize = 4096;
const SAVE_QUOTA: usize = 16 * 1024;
const ERROR_DISK_FULL: u32 = 112;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const CAP_LOCAL_SETTINGS: u64 = 1 << 0;
const CAP_LOCAL_SAVES: u64 = 1 << 1;
const CAP_LOCAL_PROFILES: u64 = 1 << 2;
const CAP_PROFILE_NOTIFICATIONS: u64 = 1 << 3;
const MAX_PROFILES: u64 = 8;
const MAX_NOTIFICATIONS: usize = 16;

#[derive(Default)]
pub(super) struct PlatformFixture {
    active: bool,
    settings: BTreeMap<u32, u64>,
    saves: BTreeMap<u32, Vec<u8>>,
    profile_count: u64,
    selected_profile: u64,
    profile_changes: VecDeque<(u64, u64)>,
}

impl PlatformFixture {
    fn used_save_bytes(&self) -> usize {
        self.saves.values().map(Vec::len).sum()
    }
}

pub(super) fn register(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    for (symbol, active) in [
        ("OpenFixturePlatform", true),
        ("CloseFixturePlatform", false),
    ] {
        let state = Arc::clone(state);
        registry.register(MODULE, symbol, move |_, cpu, _| {
            let mut platform = state.platform_fixture.lock().expect("fixture platform");
            if platform.active == active {
                return state.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
            }
            platform.active = active;
            Ok(1)
        });
    }

    let identity = Arc::clone(state);
    registry.register(MODULE, "GetFixtureApplicationId", move |_, cpu, _| {
        let platform = identity.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return identity.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        Ok(FIXTURE_APP_ID)
    });

    let set = Arc::clone(state);
    registry.register(MODULE, "SetFixtureSetting", move |args, cpu, _| {
        let mut platform = set.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return set.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        let key = args.rcx as u32;
        if key == 0 {
            return set.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        if platform.settings.len() >= MAX_SETTINGS && !platform.settings.contains_key(&key) {
            return set.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        platform.settings.insert(key, args.rdx);
        Ok(1)
    });

    let get = Arc::clone(state);
    registry.register(MODULE, "GetFixtureSetting", move |args, cpu, memory| {
        let platform = get.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return get.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        let key = args.rcx as u32;
        if key == 0 {
            return get.fail(cpu.gs_base, ERROR_INVALID_PARAMETER, 0);
        }
        let Some(&value) = platform.settings.get(&key) else {
            return get.fail(cpu.gs_base, ERROR_NOT_FOUND, 0);
        };
        if args.rdx == 0 || memory.write_u64(args.rdx, value).is_err() {
            return get.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        Ok(1)
    });
    register_saves(registry, state);
    register_profiles(registry, state);

    let capabilities = Arc::clone(state);
    registry.register(
        MODULE,
        "GetFixturePlatformCapabilities",
        move |_, cpu, _| {
            let platform = capabilities
                .platform_fixture
                .lock()
                .expect("fixture platform");
            if !platform.active {
                return capabilities.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
            }
            Ok(CAP_LOCAL_SETTINGS
                | CAP_LOCAL_SAVES
                | CAP_LOCAL_PROFILES
                | CAP_PROFILE_NOTIFICATIONS)
        },
    );
}

fn register_saves(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let quota = Arc::clone(state);
    registry.register(MODULE, "GetFixtureSaveQuota", move |args, cpu, memory| {
        let platform = quota.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return quota.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        // Both values are byte counts. Write the whole record or leave it untouched.
        let mut record = [0; 16];
        record[..8].copy_from_slice(&(SAVE_QUOTA as u64).to_le_bytes());
        record[8..].copy_from_slice(&(platform.used_save_bytes() as u64).to_le_bytes());
        if args.rcx == 0 || memory.write_bytes(args.rcx, &record).is_err() {
            return quota.fail(cpu.gs_base, ERROR_NOACCESS, 0);
        }
        Ok(1)
    });

    let save = Arc::clone(state);
    registry.register(MODULE, "WriteFixtureSave", move |args, cpu, memory| {
        let mut platform = save.platform_fixture.lock().expect("fixture platform");
        let fail = |error| save.fail(cpu.gs_base, error, 0);
        if !platform.active {
            return fail(ERROR_INVALID_STATE);
        }
        let slot = args.rcx as u32;
        let size = args.r8;
        if slot == 0 || !(1..=MAX_SAVE_BYTES as u64).contains(&size) {
            return fail(ERROR_INVALID_PARAMETER);
        }
        let previous = platform.saves.get(&slot);
        let used = platform.used_save_bytes();
        let size = size as usize;
        if (previous.is_none() && platform.saves.len() == MAX_SAVE_SLOTS)
            || used - previous.map_or(0, Vec::len) + size > SAVE_QUOTA
        {
            return fail(ERROR_DISK_FULL);
        }
        let mut contents = vec![0; size];
        if args.rdx == 0 || memory.read_bytes(args.rdx, &mut contents).is_err() {
            return fail(ERROR_NOACCESS);
        }
        platform.saves.insert(slot, contents);
        Ok(1)
    });

    // GetFixtureSaveSize(slot: u32) -> u64 returns the payload byte count
    // without reading into guest memory or changing the save. Saves are
    // nonempty, so zero denotes failure with the same errors as ReadFixtureSave.
    let size = Arc::clone(state);
    registry.register(MODULE, "GetFixtureSaveSize", move |args, cpu, _| {
        let platform = size.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return size.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        let Some(contents) = platform.saves.get(&(args.rcx as u32)) else {
            return size.fail(cpu.gs_base, ERROR_NOT_FOUND, 0);
        };
        Ok(contents.len() as u64)
    });

    let load = Arc::clone(state);
    registry.register(MODULE, "ReadFixtureSave", move |args, cpu, memory| {
        let platform = load.platform_fixture.lock().expect("fixture platform");
        let fail = |error| load.fail(cpu.gs_base, error, 0);
        if !platform.active {
            return fail(ERROR_INVALID_STATE);
        }
        let Some(contents) = platform.saves.get(&(args.rcx as u32)) else {
            return fail(ERROR_NOT_FOUND);
        };
        if args.r8 < contents.len() as u64 {
            return fail(ERROR_INSUFFICIENT_BUFFER);
        }
        if args.rdx == 0 || memory.write_bytes(args.rdx, contents).is_err() {
            return fail(ERROR_NOACCESS);
        }
        Ok(contents.len() as u64)
    });

    let delete = Arc::clone(state);
    registry.register(MODULE, "DeleteFixtureSave", move |args, cpu, _| {
        let mut platform = delete.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return delete.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        if platform.saves.remove(&(args.rcx as u32)).is_none() {
            return delete.fail(cpu.gs_base, ERROR_NOT_FOUND, 0);
        }
        Ok(1)
    });
}

fn register_profiles(registry: &mut ImportRegistry, state: &Arc<ServicesState>) {
    let create = Arc::clone(state);
    registry.register(MODULE, "CreateFixtureProfile", move |_, cpu, _| {
        let mut platform = create.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return create.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        if platform.profile_count == MAX_PROFILES {
            return create.fail(cpu.gs_base, ERROR_NOT_ENOUGH_MEMORY, 0);
        }
        platform.profile_count += 1;
        Ok(platform.profile_count)
    });

    let select = Arc::clone(state);
    registry.register(MODULE, "SelectFixtureProfile", move |args, cpu, _| {
        let mut platform = select.platform_fixture.lock().expect("fixture platform");
        let fail = |error| select.fail(cpu.gs_base, error, 0);
        if !platform.active {
            return fail(ERROR_INVALID_STATE);
        }
        let next = args.rcx;
        if next > platform.profile_count {
            return fail(ERROR_NOT_FOUND);
        }
        let previous = platform.selected_profile;
        if previous != next {
            if platform.profile_changes.len() == MAX_NOTIFICATIONS {
                return fail(ERROR_NOT_ENOUGH_MEMORY);
            }
            platform.profile_changes.push_back((previous, next));
            platform.selected_profile = next;
        }
        Ok(1)
    });

    let selected = Arc::clone(state);
    registry.register(MODULE, "GetSelectedFixtureProfile", move |_, cpu, _| {
        let platform = selected.platform_fixture.lock().expect("fixture platform");
        if !platform.active {
            return selected.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
        }
        Ok(platform.selected_profile)
    });

    let poll = Arc::clone(state);
    registry.register(
        MODULE,
        "PollFixtureProfileChange",
        move |args, cpu, memory| {
            let mut platform = poll.platform_fixture.lock().expect("fixture platform");
            if !platform.active {
                return poll.fail(cpu.gs_base, ERROR_INVALID_STATE, 0);
            }
            let Some(&(previous, next)) = platform.profile_changes.front() else {
                return Ok(0);
            };
            let mut record = [0; 16];
            record[..8].copy_from_slice(&previous.to_le_bytes());
            record[8..].copy_from_slice(&next.to_le_bytes());
            if args.rcx == 0 || memory.write_bytes(args.rcx, &record).is_err() {
                return poll.fail(cpu.gs_base, ERROR_NOACCESS, 0);
            }
            platform.profile_changes.pop_front();
            Ok(1)
        },
    );
}
