// SPDX-License-Identifier: GPL-3.0-or-later

//! A deterministic, non-executing layout planner for explicit PE32+ modules.
//!
//! The planner chooses image base addresses inside a caller-supplied arena. Its
//! input is arithmetic metadata only: each image carries the preferred base,
//! `SizeOfImage` and relocations-stripped state derived from its parsed PE
//! headers. It does not allocate, reserve, map or write guest memory, relocate
//! or bind imports, register services, initialize modules or execute guest
//! code. The result is inspection data for a later transactional memory owner.
//!
//! Microsoft's `IMAGE_OPTIONAL_HEADER64` documentation describes `ImageBase`
//! as the preferred address of the first byte of the image and says that it is
//! a multiple of 64K bytes:
//! <https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-image_optional_header64>.
//! Microsoft's `IMAGE_FILE_HEADER` documentation says that
//! `IMAGE_FILE_RELOCS_STRIPPED` means the file must be loaded at its preferred
//! base address, with an error if that base is unavailable:
//! <https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-image_file_header>.
//! This planner therefore accepts only 64 KiB-aligned image bases. A
//! relocatable image whose preferred base is unsuitable falls back to the
//! lowest free aligned base in the arena; a relocation-stripped image has no
//! such fallback.
//!
//! Occupied intervals are half-open `[start, end)` byte ranges. They may be
//! supplied in any order. The planner rejects a reversed interval and any
//! overlap of two nonempty intervals, sorts a local copy by address for
//! collision tests, and treats empty intervals as no-ops. Adjacent intervals do
//! not overlap. The arena is also half-open, `[base, base + size)`; its size
//! may be zero, in which case every nonzero image is too large for it, but the
//! checked end address must be representable.

use std::fmt;

use crate::pe::ParsedPe;

/// Required alignment of a PE32+ image base, in bytes.
pub const IMAGE_BASE_ALIGNMENT: u64 = 0x1_0000;
const IMAGE_BASE_ALIGNMENT_MASK: u64 = IMAGE_BASE_ALIGNMENT - 1;
/// COFF characteristics bit 0. `reloc.rs` uses the same public PE format value
/// internally; it is duplicated here so this arithmetic layer does not depend
/// on the relocation writer.
const IMAGE_FILE_RELOCS_STRIPPED: u16 = 0x0001;

/// A half-open guest address interval `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddressInterval {
    pub start: u64,
    pub end: u64,
}

impl AddressInterval {
    pub const fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }
}

/// The caller-supplied half-open arena `[base, base + size)`.
///
/// The base does not itself have to be image-aligned. Placement candidates
/// inside the arena are always rounded up to [`IMAGE_BASE_ALIGNMENT`]. A zero
/// size describes an empty arena, not malformed input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleLayoutArena {
    pub base: u64,
    pub size: u64,
}

/// PE32+ metadata needed to place one explicit module image.
///
/// `size_of_image` is the exact nonzero byte extent used for collision tests;
/// the planner does not infer section alignment or inspect section contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleImageDescriptor {
    pub preferred_base: u64,
    pub size_of_image: u32,
    pub relocations_stripped: bool,
}

impl ModuleImageDescriptor {
    /// Derive the arithmetic descriptor from parsed PE32+ metadata.
    pub fn from_parsed_pe(pe: &ParsedPe) -> Self {
        Self {
            preferred_base: pe.image_base,
            size_of_image: pe.size_of_image,
            relocations_stripped: pe.characteristics & IMAGE_FILE_RELOCS_STRIPPED != 0,
        }
    }
}

/// Input-shape errors that prevent any layout plan from being constructed.
/// Image-specific failures are retained per image instead of aborting the plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleLayoutError {
    /// `base + size` is not representable as a u64.
    ArenaEndOverflow { base: u64, size: u64 },
    /// An occupied interval has `end < start`. Empty intervals are valid and
    /// occupy no addresses.
    InvalidOccupiedInterval {
        index: usize,
        interval: AddressInterval,
    },
    /// Two nonempty occupied intervals overlap. `first` and `second` are in
    /// address order; their indexes are positions in the caller's input.
    OverlappingOccupiedIntervals {
        first_index: usize,
        first: AddressInterval,
        second_index: usize,
        second: AddressInterval,
        overlap: AddressInterval,
    },
}

impl fmt::Display for ModuleLayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArenaEndOverflow { base, size } => write!(
                f,
                "module layout arena end overflows: base {base:#x}, size {size:#x}"
            ),
            Self::InvalidOccupiedInterval { index, interval } => write!(
                f,
                "occupied interval {index} has end before start: [{:#x}, {:#x})",
                interval.start, interval.end
            ),
            Self::OverlappingOccupiedIntervals {
                first_index,
                second_index,
                overlap,
                ..
            } => write!(
                f,
                "occupied intervals {first_index} and {second_index} overlap at [{:#x}, {:#x})",
                overlap.start, overlap.end
            ),
        }
    }
}

impl std::error::Error for ModuleLayoutError {}

/// Why an image's preferred base cannot be used. Checks are performed in the
/// order of the variants listed here, so an unaligned or overflowing base is
/// not also reported as outside the arena or overlapping another range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreferredBaseRejection {
    /// The preferred base is not a multiple of [`IMAGE_BASE_ALIGNMENT`].
    Unaligned { preferred_base: u64 },
    /// `preferred_base + size_of_image` is not representable as a u64.
    EndOverflow {
        preferred_base: u64,
        image_size: u32,
    },
    /// The checked preferred image span is not wholly inside the arena.
    OutsideArena {
        image: AddressInterval,
        arena: AddressInterval,
    },
    /// The checked preferred image span overlaps a caller-supplied occupied
    /// interval. Half-open adjacency is not an overlap.
    OverlapsOccupied {
        occupied_index: usize,
        occupied: AddressInterval,
        image: AddressInterval,
    },
    /// The checked preferred image span overlaps a module successfully placed
    /// earlier in the explicit input order.
    OverlapsPlacedImage {
        placed_image_index: usize,
        placed: AddressInterval,
        image: AddressInterval,
    },
}

impl fmt::Display for PreferredBaseRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unaligned { preferred_base } => write!(
                f,
                "preferred base {preferred_base:#x} is not {IMAGE_BASE_ALIGNMENT}-byte aligned"
            ),
            Self::EndOverflow {
                preferred_base,
                image_size,
            } => write!(
                f,
                "preferred image end overflows: base {preferred_base:#x}, size {image_size:#x}"
            ),
            Self::OutsideArena { image, arena } => write!(
                f,
                "preferred image span [{:#x}, {:#x}) is outside arena [{:#x}, {:#x})",
                image.start, image.end, arena.start, arena.end
            ),
            Self::OverlapsOccupied {
                occupied_index,
                occupied,
                image,
            } => write!(
                f,
                "preferred image span [{:#x}, {:#x}) overlaps occupied interval {occupied_index} [{:#x}, {:#x})",
                image.start, image.end, occupied.start, occupied.end
            ),
            Self::OverlapsPlacedImage {
                placed_image_index,
                placed,
                image,
            } => write!(
                f,
                "preferred image span [{:#x}, {:#x}) overlaps image {placed_image_index} at [{:#x}, {:#x})",
                image.start, image.end, placed.start, placed.end
            ),
        }
    }
}

/// Why one image could not be placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageLayoutFailure {
    /// PE `SizeOfImage` was zero. The planner has no meaningful span to test.
    ZeroImageSize,
    /// A relocatable image is larger than the whole arena, so no base choice
    /// can make it fit. For a relocation-stripped image the fixed-base failure
    /// is reported instead because only its preferred base is admissible.
    ImageTooLargeForArena { image_size: u64, arena_size: u64 },
    /// A relocation-stripped image cannot use its preferred base. Microsoft's
    /// COFF documentation gives such an image no fallback base.
    FixedBaseUnavailable { reason: PreferredBaseRejection },
    /// A relocatable image could use neither its preferred base nor any other
    /// free aligned base wholly inside the arena.
    NoFreeAlignedBase {
        preferred_rejection: PreferredBaseRejection,
    },
}

impl fmt::Display for ImageLayoutFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroImageSize => f.write_str("image SizeOfImage is zero"),
            Self::ImageTooLargeForArena {
                image_size,
                arena_size,
            } => write!(
                f,
                "image size {image_size:#x} exceeds arena size {arena_size:#x}"
            ),
            Self::FixedBaseUnavailable { reason } => {
                write!(f, "fixed image cannot use its preferred base: {reason}")
            }
            Self::NoFreeAlignedBase {
                preferred_rejection,
            } => write!(
                f,
                "no free aligned image base is available; preferred base was rejected because {preferred_rejection}"
            ),
        }
    }
}

impl std::error::Error for ImageLayoutFailure {}

/// A checked image placement. `end` is exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImagePlacement {
    pub base: u64,
    pub end: u64,
    pub used_preferred_base: bool,
}

/// Placement result for one input image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageLayoutOutcome {
    Placed(ImagePlacement),
    Failed(ImageLayoutFailure),
}

/// The input descriptor and outcome for one explicit image, retaining the
/// caller's package order through `index`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModuleImageLayout {
    pub index: usize,
    pub descriptor: ModuleImageDescriptor,
    pub outcome: ImageLayoutOutcome,
}

/// A deterministic layout plan for an explicit module batch. Build it with
/// [`ModuleLayoutPlan::build`]. Images are reported in input order whether or
/// not they were placed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleLayoutPlan {
    arena: AddressInterval,
    images: Vec<ModuleImageLayout>,
}

impl ModuleLayoutPlan {
    /// Plan every image in input order. A failed image reserves no addresses;
    /// later images are still planned and their successful placements retained.
    ///
    /// The caller's occupied intervals do not need to be sorted. Nonempty
    /// intervals must not overlap one another. After validation they and the
    /// placements made so far are kept in address order internally, so a
    /// collision answer does not depend on the order in which the caller
    /// supplied occupied ranges or on the insertion representation of earlier placements.
    pub fn build(
        arena: ModuleLayoutArena,
        occupied: &[AddressInterval],
        images: &[ModuleImageDescriptor],
    ) -> Result<Self, ModuleLayoutError> {
        let end =
            arena
                .base
                .checked_add(arena.size)
                .ok_or(ModuleLayoutError::ArenaEndOverflow {
                    base: arena.base,
                    size: arena.size,
                })?;
        let arena_interval = AddressInterval::new(arena.base, end);
        let mut blockers = validate_occupied(occupied)?;
        let mut layouts = Vec::with_capacity(images.len());

        for (index, &descriptor) in images.iter().enumerate() {
            let outcome = plan_image(descriptor, arena_interval, &blockers);
            if let ImageLayoutOutcome::Placed(placement) = outcome {
                insert_blocker(
                    &mut blockers,
                    Blocker::Placed {
                        image_index: index,
                        interval: AddressInterval::new(placement.base, placement.end),
                    },
                );
            }
            layouts.push(ModuleImageLayout {
                index,
                descriptor,
                outcome,
            });
        }

        Ok(Self {
            arena: arena_interval,
            images: layouts,
        })
    }

    /// The checked half-open arena interval.
    pub fn arena(&self) -> AddressInterval {
        self.arena
    }

    /// Per-image outcomes in the caller's explicit input order.
    pub fn images(&self) -> &[ModuleImageLayout] {
        &self.images
    }

    /// Successful placements with their input indexes, in input order.
    pub fn placements(&self) -> impl Iterator<Item = (usize, &ImagePlacement)> + '_ {
        self.images.iter().filter_map(|image| match &image.outcome {
            ImageLayoutOutcome::Placed(placement) => Some((image.index, placement)),
            ImageLayoutOutcome::Failed(_) => None,
        })
    }

    /// Whether every image was placed.
    pub fn all_placed(&self) -> bool {
        self.images
            .iter()
            .all(|image| matches!(image.outcome, ImageLayoutOutcome::Placed(_)))
    }

    /// Base addresses in input order when every image was placed. This is the
    /// shape a later module binding planner consumes; `None` explicitly means
    /// at least one image has no base.
    pub fn base_addresses(&self) -> Option<Vec<u64>> {
        self.images
            .iter()
            .map(|image| match image.outcome {
                ImageLayoutOutcome::Placed(placement) => Some(placement.base),
                ImageLayoutOutcome::Failed(_) => None,
            })
            .collect()
    }
}

#[derive(Clone, Copy)]
enum Blocker {
    Occupied {
        index: usize,
        interval: AddressInterval,
    },
    Placed {
        image_index: usize,
        interval: AddressInterval,
    },
}

impl Blocker {
    fn interval(self) -> AddressInterval {
        match self {
            Self::Occupied { interval, .. } | Self::Placed { interval, .. } => interval,
        }
    }

    fn rejection(self, image: AddressInterval) -> PreferredBaseRejection {
        match self {
            Self::Occupied { index, interval } => PreferredBaseRejection::OverlapsOccupied {
                occupied_index: index,
                occupied: interval,
                image,
            },
            Self::Placed {
                image_index,
                interval,
            } => PreferredBaseRejection::OverlapsPlacedImage {
                placed_image_index: image_index,
                placed: interval,
                image,
            },
        }
    }
}

fn blocker_key(blocker: Blocker) -> (u64, u64) {
    let interval = blocker.interval();
    (interval.start, interval.end)
}

#[derive(Clone, Copy)]
struct OccupiedBlock {
    index: usize,
    interval: AddressInterval,
}

/// Validate interval shape and overlap, then return nonempty occupied ranges
/// in address order. Empty ranges cannot collide and are discarded.
fn validate_occupied(occupied: &[AddressInterval]) -> Result<Vec<Blocker>, ModuleLayoutError> {
    let mut blocks = Vec::with_capacity(occupied.len());
    for (index, &interval) in occupied.iter().enumerate() {
        if interval.end < interval.start {
            return Err(ModuleLayoutError::InvalidOccupiedInterval { index, interval });
        }
        if interval.start != interval.end {
            blocks.push(OccupiedBlock { index, interval });
        }
    }
    blocks.sort_by_key(|block| (block.interval.start, block.interval.end, block.index));

    for pair in blocks.windows(2) {
        let first = pair[0];
        let second = pair[1];
        if second.interval.start < first.interval.end {
            return Err(ModuleLayoutError::OverlappingOccupiedIntervals {
                first_index: first.index,
                first: first.interval,
                second_index: second.index,
                second: second.interval,
                overlap: AddressInterval::new(
                    first.interval.start.max(second.interval.start),
                    first.interval.end.min(second.interval.end),
                ),
            });
        }
    }
    Ok(blocks
        .into_iter()
        .map(|block| Blocker::Occupied {
            index: block.index,
            interval: block.interval,
        })
        .collect())
}

fn insert_blocker(blockers: &mut Vec<Blocker>, blocker: Blocker) {
    let key = blocker_key(blocker);
    let position = blockers.partition_point(|&existing| blocker_key(existing) < key);
    blockers.insert(position, blocker);
}

fn plan_image(
    descriptor: ModuleImageDescriptor,
    arena: AddressInterval,
    blockers: &[Blocker],
) -> ImageLayoutOutcome {
    if descriptor.size_of_image == 0 {
        return ImageLayoutOutcome::Failed(ImageLayoutFailure::ZeroImageSize);
    }
    let image_size = u64::from(descriptor.size_of_image);
    let preferred = check_preferred_base(descriptor, arena, blockers);

    if descriptor.relocations_stripped {
        return match preferred {
            Ok(placement) => ImageLayoutOutcome::Placed(placement),
            Err(reason) => {
                ImageLayoutOutcome::Failed(ImageLayoutFailure::FixedBaseUnavailable { reason })
            }
        };
    }

    // `arena` is constructed from checked base+size arithmetic, so this
    // subtraction cannot underflow.
    let arena_size = arena.end - arena.start;
    if image_size > arena_size {
        return ImageLayoutOutcome::Failed(ImageLayoutFailure::ImageTooLargeForArena {
            image_size,
            arena_size,
        });
    }

    let preferred_rejection = match preferred {
        Ok(placement) => return ImageLayoutOutcome::Placed(placement),
        Err(reason) => reason,
    };
    if let Some(base) = lowest_free_aligned_base(arena, image_size, blockers)
        && let Some(end) = base.checked_add(image_size)
    {
        return ImageLayoutOutcome::Placed(ImagePlacement {
            base,
            end,
            used_preferred_base: false,
        });
    }

    ImageLayoutOutcome::Failed(ImageLayoutFailure::NoFreeAlignedBase {
        preferred_rejection,
    })
}

fn check_preferred_base(
    descriptor: ModuleImageDescriptor,
    arena: AddressInterval,
    blockers: &[Blocker],
) -> Result<ImagePlacement, PreferredBaseRejection> {
    let preferred_base = descriptor.preferred_base;
    if !preferred_base.is_multiple_of(IMAGE_BASE_ALIGNMENT) {
        return Err(PreferredBaseRejection::Unaligned { preferred_base });
    }
    let image_size = u64::from(descriptor.size_of_image);
    let Some(end) = preferred_base.checked_add(image_size) else {
        return Err(PreferredBaseRejection::EndOverflow {
            preferred_base,
            image_size: descriptor.size_of_image,
        });
    };
    let image = AddressInterval::new(preferred_base, end);
    if preferred_base < arena.start || end > arena.end {
        return Err(PreferredBaseRejection::OutsideArena { image, arena });
    }
    if let Some(blocker) = first_overlap(blockers, image) {
        return Err(blocker.rejection(image));
    }
    Ok(ImagePlacement {
        base: preferred_base,
        end,
        used_preferred_base: true,
    })
}

/// Find the first occupied or already-placed interval overlapping `image`.
/// The blockers are sorted by start, so scanning can stop at the first one
/// that starts at or after the image end.
fn first_overlap(blockers: &[Blocker], image: AddressInterval) -> Option<Blocker> {
    for &blocker in blockers {
        let interval = blocker.interval();
        if interval.end <= image.start {
            continue;
        }
        if interval.start >= image.end {
            break;
        }
        return Some(blocker);
    }
    None
}

/// Sweep disjoint sorted blockers rather than candidate alignment slots, so
/// the cost depends on the number of intervals rather than the arena size.
fn lowest_free_aligned_base(
    arena: AddressInterval,
    image_size: u64,
    blockers: &[Blocker],
) -> Option<u64> {
    let arena_size = arena.end.checked_sub(arena.start)?;
    if image_size > arena_size {
        return None;
    }
    let latest_base = arena.end.checked_sub(image_size)?;
    let mut candidate = align_up(arena.start)?;

    for &blocker in blockers {
        if candidate > latest_base {
            return None;
        }
        let interval = blocker.interval();
        if interval.end <= candidate {
            continue;
        }
        if interval.start <= candidate {
            candidate = align_up(interval.end)?;
            continue;
        }

        let candidate_end = candidate.checked_add(image_size)?;
        if candidate_end <= interval.start {
            return Some(candidate);
        }
        // No aligned base before this blocker's end can avoid it: the gap is
        // smaller than the image, and a base inside the interval collides.
        candidate = align_up(interval.end)?;
    }

    (candidate <= latest_base).then_some(candidate)
}

fn align_up(address: u64) -> Option<u64> {
    Some(address.checked_add(IMAGE_BASE_ALIGNMENT_MASK)? & !IMAGE_BASE_ALIGNMENT_MASK)
}
