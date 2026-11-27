// SPDX-License-Identifier: GPL-3.0-or-later

//! Original synthetic arithmetic cases for the non-executing module layout
//! planner. These tests use numeric image descriptors only: no host library,
//! proprietary image, guest memory or execution takes part.

use xenora_loader::ParsedPe;
use xenora_loader::module_layout::{
    AddressInterval, ImageLayoutFailure, ImageLayoutOutcome, ImagePlacement, ModuleImageDescriptor,
    ModuleLayoutArena, ModuleLayoutError, ModuleLayoutPlan, PreferredBaseRejection,
};

const ALIGNMENT: u64 = 0x1_0000;
const ARENA_BASE: u64 = 0x0000_0007_0000_0000;
const ARENA_SIZE: u64 = 0x10_0000;
const PREFERRED: u64 = ARENA_BASE + 0x4_0000;
const HIGHEST_ALIGNED_BASE: u64 = u64::MAX - (ALIGNMENT - 1);

fn arena() -> ModuleLayoutArena {
    ModuleLayoutArena {
        base: ARENA_BASE,
        size: ARENA_SIZE,
    }
}

fn arena_at(base: u64, size: u64) -> ModuleLayoutArena {
    ModuleLayoutArena { base, size }
}

fn image(
    preferred_base: u64,
    size_of_image: u32,
    relocations_stripped: bool,
) -> ModuleImageDescriptor {
    ModuleImageDescriptor {
        preferred_base,
        size_of_image,
        relocations_stripped,
    }
}

fn plan(images: &[ModuleImageDescriptor]) -> ModuleLayoutPlan {
    ModuleLayoutPlan::build(arena(), &[], images).expect("valid planner input")
}

fn placement(plan: &ModuleLayoutPlan, index: usize) -> ImagePlacement {
    match plan.images()[index].outcome {
        ImageLayoutOutcome::Placed(placement) => placement,
        ImageLayoutOutcome::Failed(failure) => {
            panic!("image {index} failed instead of being placed: {failure}")
        }
    }
}

fn failure(plan: &ModuleLayoutPlan, index: usize) -> ImageLayoutFailure {
    match plan.images()[index].outcome {
        ImageLayoutOutcome::Failed(failure) => failure,
        ImageLayoutOutcome::Placed(placement) => {
            panic!(
                "image {index} was placed at {:#x} instead of failing",
                placement.base
            )
        }
    }
}

#[test]
fn descriptor_is_derived_from_parsed_pe_metadata() {
    let parsed = ParsedPe {
        machine: 0x8664,
        number_of_sections: 0,
        time_date_stamp: 0,
        characteristics: 0x2001,
        image_base: PREFERRED,
        section_alignment: 0x1000,
        file_alignment: 0x200,
        size_of_image: 0x3000,
        size_of_headers: 0x200,
        entry_point_rva: 0,
        sections: Vec::new(),
    };

    let descriptor = ModuleImageDescriptor::from_parsed_pe(&parsed);
    assert_eq!(descriptor.preferred_base, PREFERRED);
    assert_eq!(descriptor.size_of_image, 0x3000);
    assert!(descriptor.relocations_stripped);

    let mut relocatable = parsed;
    relocatable.characteristics &= !1;
    assert!(!ModuleImageDescriptor::from_parsed_pe(&relocatable).relocations_stripped);
}

#[test]
fn preferred_base_is_used_when_aligned_inside_and_unoccupied() {
    let plan = plan(&[image(PREFERRED, 0x9000, false)]);

    assert!(plan.all_placed());
    assert_eq!(plan.base_addresses(), Some(vec![PREFERRED]));
    assert_eq!(
        placement(&plan, 0),
        ImagePlacement {
            base: PREFERRED,
            end: PREFERRED + 0x9000,
            used_preferred_base: true,
        }
    );
}

#[test]
fn relocatable_image_falls_back_to_the_lowest_free_aligned_base() {
    let occupied = [AddressInterval::new(PREFERRED, PREFERRED + 0x1000)];
    let plan = ModuleLayoutPlan::build(arena(), &occupied, &[image(PREFERRED, 0x8000, false)])
        .expect("valid planner input");

    assert_eq!(
        placement(&plan, 0),
        ImagePlacement {
            base: ARENA_BASE,
            end: ARENA_BASE + 0x8000,
            used_preferred_base: false,
        }
    );
}

#[test]
fn fallback_rounds_an_unaligned_arena_base_up_to_the_image_alignment() {
    let unaligned_base = ARENA_BASE + 0x123;
    let first_aligned = ARENA_BASE + ALIGNMENT;
    let arena = arena_at(unaligned_base, 0x3_0000);
    let plan = ModuleLayoutPlan::build(arena, &[], &[image(0x123, 0x8000, false)])
        .expect("an unaligned arena boundary is valid input");

    assert_eq!(placement(&plan, 0).base, first_aligned);
    assert!(!placement(&plan, 0).used_preferred_base);
}

#[test]
fn image_can_end_exactly_at_the_arena_end() {
    let exact = arena_at(ARENA_BASE, 0x2_0000);
    let base = ARENA_BASE + ALIGNMENT;
    let plan = ModuleLayoutPlan::build(exact, &[], &[image(base, 0x1_0000, false)])
        .expect("exact end boundary is inside the half-open arena");

    assert_eq!(placement(&plan, 0).end, ARENA_BASE + 0x2_0000);
    assert!(plan.all_placed());
}

#[test]
fn arena_end_overflow_is_an_input_error() {
    let malformed = arena_at(HIGHEST_ALIGNED_BASE, 0x2_0000);
    assert_eq!(
        ModuleLayoutPlan::build(malformed, &[], &[]).unwrap_err(),
        ModuleLayoutError::ArenaEndOverflow {
            base: HIGHEST_ALIGNED_BASE,
            size: 0x2_0000,
        }
    );
}

#[test]
fn empty_arena_is_valid_shape_but_no_nonzero_image_fits_it() {
    let empty = arena_at(ARENA_BASE, 0);
    let plan = ModuleLayoutPlan::build(empty, &[], &[image(ARENA_BASE, 1, false)])
        .expect("an empty half-open arena is well formed");

    assert_eq!(plan.arena(), AddressInterval::new(ARENA_BASE, ARENA_BASE));
    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::ImageTooLargeForArena {
            image_size: 1,
            arena_size: 0,
        }
    );
}

#[test]
fn adjacent_occupied_intervals_do_not_overlap() {
    let occupied = [
        AddressInterval::new(ARENA_BASE, ARENA_BASE + ALIGNMENT),
        AddressInterval::new(ARENA_BASE + ALIGNMENT, ARENA_BASE + 2 * ALIGNMENT),
    ];
    let plan = ModuleLayoutPlan::build(arena(), &occupied, &[image(0x77, 0x1000, false)])
        .expect("half-open adjacency is not overlap");

    assert_eq!(placement(&plan, 0).base, ARENA_BASE + 2 * ALIGNMENT);
}

#[test]
fn one_byte_of_overlap_with_an_occupied_interval_is_a_collision() {
    let occupied_start = ARENA_BASE + ALIGNMENT - 1;
    let occupied = [AddressInterval::new(
        occupied_start,
        occupied_start + 0x1000,
    )];
    let images = [image(ARENA_BASE, 0x1_0000, true)];
    let plan = ModuleLayoutPlan::build(arena(), &occupied, &images).expect("valid planner input");

    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::FixedBaseUnavailable {
            reason: PreferredBaseRejection::OverlapsOccupied {
                occupied_index: 0,
                occupied: occupied[0],
                image: AddressInterval::new(ARENA_BASE, ARENA_BASE + 0x1_0000),
            },
        }
    );
}

#[test]
fn unsorted_occupied_input_is_normalized_without_changing_the_plan() {
    let low = AddressInterval::new(ARENA_BASE, ARENA_BASE + 1);
    let middle = AddressInterval::new(ARENA_BASE + ALIGNMENT, ARENA_BASE + ALIGNMENT + 1);
    let high = AddressInterval::new(ARENA_BASE + 4 * ALIGNMENT, ARENA_BASE + 5 * ALIGNMENT);
    let unsorted = [high, low, middle];
    let sorted = [low, middle, high];
    let images = [image(0x55, 0x1000, false)];

    let from_unsorted = ModuleLayoutPlan::build(arena(), &unsorted, &images).unwrap();
    let from_sorted = ModuleLayoutPlan::build(arena(), &sorted, &images).unwrap();

    assert_eq!(from_unsorted, from_sorted);
    assert_eq!(
        placement(&from_unsorted, 0).base,
        ARENA_BASE + 2 * ALIGNMENT
    );
}

#[test]
fn empty_occupied_intervals_are_noops() {
    let occupied = [
        AddressInterval::new(PREFERRED, PREFERRED),
        AddressInterval::new(PREFERRED + 0x1000, PREFERRED + 0x1000),
    ];
    let plan = ModuleLayoutPlan::build(arena(), &occupied, &[image(PREFERRED, 0x1000, false)])
        .expect("empty half-open intervals occupy no addresses");

    assert_eq!(placement(&plan, 0).base, PREFERRED);
    assert!(placement(&plan, 0).used_preferred_base);
}

#[test]
fn reversed_occupied_interval_is_rejected() {
    let reversed = AddressInterval::new(8, 7);
    assert_eq!(
        ModuleLayoutPlan::build(arena(), &[reversed], &[]).unwrap_err(),
        ModuleLayoutError::InvalidOccupiedInterval {
            index: 0,
            interval: reversed,
        }
    );
}

#[test]
fn overlapping_occupied_intervals_are_rejected_even_when_unsorted() {
    let lower = AddressInterval::new(0x1000, 0x4000);
    let higher = AddressInterval::new(0x3000, 0x5000);
    let input = [higher, lower];

    assert_eq!(
        ModuleLayoutPlan::build(arena(), &input, &[]).unwrap_err(),
        ModuleLayoutError::OverlappingOccupiedIntervals {
            first_index: 1,
            first: lower,
            second_index: 0,
            second: higher,
            overlap: AddressInterval::new(0x3000, 0x4000),
        }
    );
}

#[test]
fn zero_extent_is_a_per_image_failure_and_successes_are_retained() {
    let plan = plan(&[image(PREFERRED, 0, false), image(PREFERRED, 0x1000, false)]);

    assert_eq!(plan.images().len(), 2);
    assert_eq!(failure(&plan, 0), ImageLayoutFailure::ZeroImageSize);
    assert_eq!(placement(&plan, 1).base, PREFERRED);
    assert_eq!(plan.placements().count(), 1);
    assert!(!plan.all_placed());
    assert_eq!(plan.base_addresses(), None);
}

#[test]
fn unaligned_preferred_base_falls_back_only_for_a_relocatable_image() {
    let unaligned = PREFERRED + 1;
    let plan = plan(&[
        image(unaligned, 0x1000, false),
        image(unaligned, 0x1000, true),
    ]);

    assert_eq!(placement(&plan, 0).base, ARENA_BASE);
    assert!(!placement(&plan, 0).used_preferred_base);
    assert_eq!(
        failure(&plan, 1),
        ImageLayoutFailure::FixedBaseUnavailable {
            reason: PreferredBaseRejection::Unaligned {
                preferred_base: unaligned,
            },
        }
    );
}

#[test]
fn preferred_end_overflow_is_checked_and_relocatable_images_can_still_fit() {
    let images = [
        image(HIGHEST_ALIGNED_BASE, 0x1_0000, true),
        image(HIGHEST_ALIGNED_BASE, 0x1_0000, false),
    ];
    let plan = plan(&images);

    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::FixedBaseUnavailable {
            reason: PreferredBaseRejection::EndOverflow {
                preferred_base: HIGHEST_ALIGNED_BASE,
                image_size: 0x1_0000,
            },
        }
    );
    assert_eq!(placement(&plan, 1).base, ARENA_BASE);
}

#[test]
fn fixed_image_uses_an_available_preferred_base() {
    let plan = plan(&[image(PREFERRED, 0x2000, true)]);

    assert_eq!(
        placement(&plan, 0),
        ImagePlacement {
            base: PREFERRED,
            end: PREFERRED + 0x2000,
            used_preferred_base: true,
        }
    );
}

#[test]
fn collision_with_an_earlier_placement_is_precise() {
    let arena = arena_at(PREFERRED, 0x3_0000);
    let colliding = PREFERRED + ALIGNMENT;
    let images = [
        image(PREFERRED, 0x1_1000, false),
        image(colliding, 0x1_0000, true),
        image(colliding, 0x1_0000, false),
    ];
    let plan = ModuleLayoutPlan::build(arena, &[], &images).unwrap();

    assert_eq!(placement(&plan, 0).base, PREFERRED);
    assert_eq!(
        failure(&plan, 1),
        ImageLayoutFailure::FixedBaseUnavailable {
            reason: PreferredBaseRejection::OverlapsPlacedImage {
                placed_image_index: 0,
                placed: AddressInterval::new(PREFERRED, PREFERRED + 0x1_1000),
                image: AddressInterval::new(colliding, colliding + 0x1_0000),
            },
        }
    );
    assert_eq!(placement(&plan, 2).base, PREFERRED + 2 * ALIGNMENT);
}

#[test]
fn a_failed_fixed_image_reserves_no_part_of_its_would_be_span() {
    let arena = arena_at(ARENA_BASE, 0x4_0000);
    let occupied = [AddressInterval::new(ARENA_BASE, ARENA_BASE + 1)];
    let images = [image(ARENA_BASE, 0x2_0000, true), image(1, 0x1_0000, false)];
    let plan = ModuleLayoutPlan::build(arena, &occupied, &images).unwrap();

    assert!(matches!(
        failure(&plan, 0),
        ImageLayoutFailure::FixedBaseUnavailable {
            reason: PreferredBaseRejection::OverlapsOccupied { .. }
        }
    ));
    // The first aligned slot is blocked by one occupied byte, but the second
    // is free even though the failed fixed image would have covered it.
    assert_eq!(placement(&plan, 1).base, ARENA_BASE + ALIGNMENT);
}

#[test]
fn placements_are_collision_checked_as_a_set_not_in_insertion_order() {
    let arena = arena_at(ARENA_BASE, 0x5_0000);
    let high = ARENA_BASE + 4 * ALIGNMENT;
    let images = [
        image(high, 0x1_0000, false),
        image(high, 0x1_0000, false),
        image(high, 0x1_0000, false),
    ];
    let plan = ModuleLayoutPlan::build(arena, &[], &images).unwrap();

    assert_eq!(placement(&plan, 0).base, high);
    assert_eq!(placement(&plan, 1).base, ARENA_BASE);
    assert_eq!(placement(&plan, 2).base, ARENA_BASE + ALIGNMENT);
    assert!(plan.all_placed());
}

#[test]
fn fragmented_arena_selects_the_lowest_aligned_hole() {
    let occupied = [
        AddressInterval::new(ARENA_BASE, ARENA_BASE + 1),
        AddressInterval::new(ARENA_BASE + ALIGNMENT, ARENA_BASE + ALIGNMENT + 1),
        AddressInterval::new(
            ARENA_BASE + 2 * ALIGNMENT,
            ARENA_BASE + 2 * ALIGNMENT + 0x8000,
        ),
    ];
    let plan = ModuleLayoutPlan::build(arena(), &occupied, &[image(1, 0x8000, false)])
        .expect("valid occupied input");

    assert_eq!(placement(&plan, 0).base, ARENA_BASE + 3 * ALIGNMENT);
}

#[test]
fn fragmentation_can_exhaust_every_aligned_slot() {
    let arena = arena_at(ARENA_BASE, 0x2_0000);
    let first = AddressInterval::new(ARENA_BASE, ARENA_BASE + 1);
    let second = AddressInterval::new(ARENA_BASE + ALIGNMENT, ARENA_BASE + ALIGNMENT + 1);
    let occupied = [first, second];
    let images = [image(ARENA_BASE, 0x1_0000, false)];
    let plan = ModuleLayoutPlan::build(arena, &occupied, &images).unwrap();

    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::NoFreeAlignedBase {
            preferred_rejection: PreferredBaseRejection::OverlapsOccupied {
                occupied_index: 0,
                occupied: first,
                image: AddressInterval::new(ARENA_BASE, ARENA_BASE + 0x1_0000),
            },
        }
    );
}

#[test]
fn an_equal_size_arena_can_fail_because_no_aligned_image_span_fits() {
    let arena = arena_at(ARENA_BASE + 1, 0x1_0000);
    let images = [image(ARENA_BASE, 0x1_0000, false)];
    let plan = ModuleLayoutPlan::build(arena, &[], &images).unwrap();

    assert!(matches!(
        failure(&plan, 0),
        ImageLayoutFailure::NoFreeAlignedBase {
            preferred_rejection: PreferredBaseRejection::OutsideArena { .. }
        }
    ));
}

#[test]
fn too_large_image_is_a_precise_failure_and_later_images_still_place() {
    let arena = arena_at(ARENA_BASE, 0x1_0000);
    let images = [
        image(ARENA_BASE, 0x1_0001, false),
        image(ARENA_BASE, 0x1_0000, false),
    ];
    let plan = ModuleLayoutPlan::build(arena, &[], &images).unwrap();

    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::ImageTooLargeForArena {
            image_size: 0x1_0001,
            arena_size: 0x1_0000,
        }
    );
    assert_eq!(placement(&plan, 1).end, ARENA_BASE + 0x1_0000);
}

#[test]
fn arena_can_end_at_the_highest_representable_half_open_end() {
    let high_arena = arena_at(HIGHEST_ALIGNED_BASE, ALIGNMENT - 1);
    let image_size = (ALIGNMENT - 1) as u32;
    let plan = ModuleLayoutPlan::build(
        high_arena,
        &[],
        &[image(HIGHEST_ALIGNED_BASE, image_size, true)],
    )
    .expect("base plus size is exactly u64::MAX");

    assert_eq!(plan.arena().end, u64::MAX);
    assert_eq!(placement(&plan, 0).end, u64::MAX);
}

#[test]
fn high_unaligned_arena_has_no_wrapped_fallback_base() {
    let unaligned_high_base = HIGHEST_ALIGNED_BASE + 1;
    let high_arena = arena_at(unaligned_high_base, u64::MAX - unaligned_high_base);
    let plan = ModuleLayoutPlan::build(high_arena, &[], &[image(unaligned_high_base, 1, false)])
        .expect("the arena end itself is representable");

    assert_eq!(plan.arena().end, u64::MAX);
    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::NoFreeAlignedBase {
            preferred_rejection: PreferredBaseRejection::Unaligned {
                preferred_base: unaligned_high_base,
            },
        }
    );
}

#[test]
fn relocatable_image_can_use_the_highest_aligned_fallback_exactly() {
    let high_arena = arena_at(HIGHEST_ALIGNED_BASE, ALIGNMENT - 1);
    let image_size = (ALIGNMENT - 1) as u32;
    let plan = ModuleLayoutPlan::build(high_arena, &[], &[image(1, image_size, false)])
        .expect("the highest aligned fallback end is representable");

    assert_eq!(placement(&plan, 0).base, HIGHEST_ALIGNED_BASE);
    assert_eq!(placement(&plan, 0).end, u64::MAX);
    assert!(!placement(&plan, 0).used_preferred_base);
}

#[test]
fn fixed_image_starting_at_the_arena_end_is_outside() {
    let arena_end = ARENA_BASE + ARENA_SIZE;
    let plan = plan(&[image(arena_end, 1, true)]);

    assert_eq!(
        failure(&plan, 0),
        ImageLayoutFailure::FixedBaseUnavailable {
            reason: PreferredBaseRejection::OutsideArena {
                image: AddressInterval::new(arena_end, arena_end + 1),
                arena: AddressInterval::new(ARENA_BASE, arena_end),
            },
        }
    );
}

#[test]
fn an_empty_image_batch_is_a_complete_empty_plan() {
    let plan = plan(&[]);

    assert!(plan.all_placed());
    assert_eq!(plan.base_addresses(), Some(Vec::new()));
    assert_eq!(plan.placements().count(), 0);
}

#[test]
fn repeated_builds_and_equal_preferred_bases_are_deterministic() {
    let images = [
        image(PREFERRED, 0x9000, false),
        image(PREFERRED, 0x9000, false),
    ];
    let first = plan(&images);
    let second = plan(&images);

    assert_eq!(first, second);
    assert_eq!(placement(&first, 0).base, PREFERRED);
    assert_ne!(placement(&first, 1).base, PREFERRED);
    let low = placement(&first, 0);
    let later = placement(&first, 1);
    assert!(low.end <= later.base || later.end <= low.base);
}
