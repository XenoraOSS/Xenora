// SPDX-License-Identifier: GPL-3.0-or-later

//! Guest-facing D3D12-style Graphics Runtime Interfaces for Xenora.
//!
//! Provides typed guest graphics objects, handle management, resource validation,
//! command recording, and backend execution dispatch.
//!
//! Architecturally maps guest concepts:
//! - Guest Device (`ID3D12Device` / test device) -> `GraphicsDevice`
//! - Guest Command Queue (`ID3D12CommandQueue`) -> `GraphicsCommandQueue`
//! - Guest Command Allocator (`ID3D12CommandAllocator`) -> `GraphicsCommandAllocator`
//! - Guest Command List (`ID3D12GraphicsCommandList`) -> `GraphicsCommandList`
//! - Guest Resource (Buffer/Texture) -> `GraphicsResource`
//! - Guest Swap Chain / Present -> Frame presentation & framebuffer readback.

use crate::memory::{GuestMemory, MemoryError};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_FENCES: usize = 16;
const MAX_QUEUE_WAITS: usize = 8;
const MAX_FENCE_WAIT: Duration = Duration::from_secs(1);

/// Generation-checked opaque guest graphics handle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GraphicsHandle {
    pub id: u32,
    pub generation: u32,
}

impl GraphicsHandle {
    pub const NULL: Self = Self {
        id: 0,
        generation: 0,
    };

    pub fn to_u64(self) -> u64 {
        ((self.generation as u64) << 32) | (self.id as u64)
    }

    pub fn from_u64(val: u64) -> Self {
        Self {
            id: val as u32,
            generation: (val >> 32) as u32,
        }
    }
}

/// D3D12 Command List Type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandListType {
    Direct = 0,
    Bundle = 1,
    Compute = 2,
    Copy = 3,
}

/// D3D12 Resource Dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceDimension {
    Unknown = 0,
    Buffer = 1,
    Texture1D = 2,
    Texture2D = 3,
    Texture3D = 4,
}

/// D3D12 Resource State flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResourceStates(pub u32);

impl ResourceStates {
    pub const COMMON: Self = Self(0);
    pub const VERTEX_AND_CONSTANT_BUFFER: Self = Self(0x1);
    pub const INDEX_BUFFER: Self = Self(0x2);
    pub const RENDER_TARGET: Self = Self(0x4);
    pub const UNORDERED_ACCESS: Self = Self(0x8);
    pub const DEPTH_WRITE: Self = Self(0x10);
    pub const DEPTH_READ: Self = Self(0x20);
    pub const NON_PIXEL_SHADER_RESOURCE: Self = Self(0x40);
    pub const PIXEL_SHADER_RESOURCE: Self = Self(0x80);
    pub const COPY_DEST: Self = Self(0x400);
    pub const COPY_SOURCE: Self = Self(0x800);
    pub const PRESENT: Self = Self(0);
}

/// Identity of the graphics execution backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GraphicsBackendId {
    /// Deterministic software reference rasterizer (strictly for tests/CI without Metal).
    SoftwareReference,
    /// Native Apple Silicon Metal GPU rendering pipeline.
    MetalNative,
}

/// Explicit result of graphics command execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub backend: GraphicsBackendId,
    pub pixels: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub draw_count: usize,
}

/// Typed error for graphics operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphicsError {
    InvalidDeviceHandle,
    InvalidQueueHandle,
    InvalidFenceHandle,
    FenceLimitExceeded,
    FenceTimeout {
        requested: u64,
        completed: u64,
    },
    InvalidAllocatorHandle,
    InvalidCommandListHandle,
    InvalidResourceHandle,
    ResourceLifetimeViolation,
    CommandListNotRecording,
    CommandListAlreadyClosed,
    EmptyCommandList,
    InvalidDescriptor,
    InvalidViewport,
    Memory(MemoryError),
    DeviceCreationFailure(String),
    BackendError(String),
    NativeMetalRequired,
    /// An index buffer format other than DXGI `R16_UINT` (57) or `R32_UINT` (42).
    InvalidIndexFormat(u32),
    /// An indexed draw reads past its index buffer or fetches a vertex,
    /// after applying the base vertex, outside its vertex buffer, or a flat
    /// draw reads its vertices outside the vertex buffer.
    IndexOutOfRange,
    /// A descriptor heap, view, sampler, root signature, binding or draw
    /// outside the supported subset. Nothing is drawn.
    Unsupported(&'static str),
}

impl std::fmt::Display for GraphicsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDeviceHandle => write!(f, "invalid or stale graphics device handle"),
            Self::InvalidQueueHandle => write!(f, "invalid or stale command queue handle"),
            Self::InvalidFenceHandle => write!(f, "invalid or stale graphics fence handle"),
            Self::FenceLimitExceeded => write!(f, "graphics fence or queue wait limit exceeded"),
            Self::FenceTimeout {
                requested,
                completed,
            } => {
                write!(
                    f,
                    "graphics fence timed out waiting for {requested}; completed {completed}"
                )
            }
            Self::InvalidAllocatorHandle => write!(f, "invalid or stale command allocator handle"),
            Self::InvalidCommandListHandle => write!(f, "invalid or stale command list handle"),
            Self::InvalidResourceHandle => write!(f, "invalid or stale graphics resource handle"),
            Self::ResourceLifetimeViolation => write!(f, "graphics resource lifetime violation"),
            Self::CommandListNotRecording => write!(f, "command list is not in recording state"),
            Self::CommandListAlreadyClosed => write!(f, "command list has already been closed"),
            Self::EmptyCommandList => write!(f, "attempted to submit an empty command list"),
            Self::InvalidDescriptor => write!(f, "invalid descriptor or descriptor range"),
            Self::InvalidViewport => write!(f, "invalid viewport extent or bounds"),
            Self::Memory(e) => write!(f, "guest memory error: {e}"),
            Self::DeviceCreationFailure(msg) => write!(f, "device creation failure: {msg}"),
            Self::BackendError(msg) => write!(f, "metal backend error: {msg}"),
            Self::NativeMetalRequired => write!(
                f,
                "native Metal backend required but not provided or failed"
            ),
            Self::InvalidIndexFormat(format) => {
                write!(f, "unsupported index buffer format {format}")
            }
            Self::IndexOutOfRange => write!(f, "draw outside its index or vertex buffer"),
            Self::Unsupported(what) => write!(f, "unsupported graphics operation: {what}"),
        }
    }
}

impl std::error::Error for GraphicsError {}

impl From<MemoryError> for GraphicsError {
    fn from(e: MemoryError) -> Self {
        Self::Memory(e)
    }
}

/// Resource description for buffer/texture creation.
#[derive(Clone, Debug)]
pub struct ResourceDesc {
    pub dimension: ResourceDimension,
    pub width: u64,
    pub height: u32,
    pub depth_or_array_size: u16,
    pub mip_levels: u16,
    pub format_rgba8: bool,
}

/// Guest Graphics Resource.
#[derive(Clone, Debug)]
pub struct GraphicsResource {
    pub handle: GraphicsHandle,
    /// The device that created this resource.
    pub device: GraphicsHandle,
    pub desc: ResourceDesc,
    pub state: ResourceStates,
    pub guest_backing_addr: Option<u64>,
    pub data: Vec<u8>,
    /// The texel format of a `Texture2D` that a shader resource view may
    /// read; `None` for buffers and textures without a supported format.
    pub texture_format: Option<TextureFormat>,
}

/// Index size of an index buffer view, as `D3D12_INDEX_BUFFER_VIEW::Format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndexFormat {
    R16Uint,
    R32Uint,
}

impl IndexFormat {
    /// The format for a DXGI format code: `R16_UINT` (57) or `R32_UINT` (42).
    pub const fn from_dxgi(format: u32) -> Option<Self> {
        match format {
            57 => Some(Self::R16Uint),
            42 => Some(Self::R32Uint),
            _ => None,
        }
    }

    /// Bytes per index.
    pub const fn size(self) -> usize {
        match self {
            Self::R16Uint => 2,
            Self::R32Uint => 4,
        }
    }
}

/// Descriptors one shader-visible heap may hold.
pub const MAX_DESCRIPTORS_PER_HEAP: u32 = 64;
/// Largest width and height of a texture that a shader resource view reads.
pub const MAX_TEXTURE_DIMENSION: u32 = 64;
/// `GetDescriptorHandleIncrementSize` for both supported heap types. Guests
/// must query it rather than assume a value, as on D3D12.
pub const DESCRIPTOR_HANDLE_INCREMENT: u32 = 32;
/// `D3D12_DESCRIPTOR_RANGE_OFFSET_APPEND`.
pub const DESCRIPTOR_RANGE_OFFSET_APPEND: u32 = 0xffff_ffff;
/// DXGI format code of `R8G8B8A8_UNORM`.
pub const DXGI_FORMAT_R8G8B8A8_UNORM: u32 = 28;
/// DXGI format code of `BC1_UNORM`.
pub const DXGI_FORMAT_BC1_UNORM: u32 = 71;
/// `D3D12_FILTER_MIN_MAG_MIP_POINT`.
pub const D3D12_FILTER_MIN_MAG_MIP_POINT: u32 = 0;
/// `D3D12_TEXTURE_ADDRESS_MODE_CLAMP`.
pub const D3D12_TEXTURE_ADDRESS_MODE_CLAMP: u32 = 3;

/// Largest image the software reference draws for a textured draw.
pub const MAX_SOFTWARE_TEXTURED_PIXELS: usize = 4096;
/// The read-only `D3D12_RESOURCE_STATES`: vertex and constant buffer, index
/// buffer, depth read, both shader resources, indirect argument and copy source.
const READ_STATES: u32 = 0x1 | 0x2 | 0x20 | 0x40 | 0x80 | 0x200 | 0x800;

/// Bit that separates GPU descriptor handles from CPU ones.
const GPU_DESCRIPTOR_SPACE: u64 = 1 << 63;

/// `D3D12_DESCRIPTOR_HEAP_TYPE`; only the two shader-visible types exist here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorHeapType {
    CbvSrvUav,
    Sampler,
}

impl DescriptorHeapType {
    /// The heap type for a `D3D12_DESCRIPTOR_HEAP_TYPE` value: `CBV_SRV_UAV`
    /// (0) or `SAMPLER` (1). `RTV` (2) and `DSV` (3) are not supported.
    pub const fn from_d3d12(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::CbvSrvUav),
            1 => Some(Self::Sampler),
            _ => None,
        }
    }
}

/// `D3D12_CPU_DESCRIPTOR_HANDLE`: the destination of descriptor creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuDescriptorHandle(pub u64);

/// `D3D12_GPU_DESCRIPTOR_HANDLE`: the base of a bound descriptor table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuDescriptorHandle(pub u64);

/// `handle + index × increment`, or `None` when the sum leaves the heap's
/// 32-bit offset field and would carry into its heap ID.
const fn offset_handle(handle: u64, index: u32) -> Option<u64> {
    let offset = (handle as u32 as u64) + index as u64 * DESCRIPTOR_HANDLE_INCREMENT as u64;
    if offset > u32::MAX as u64 {
        None
    } else {
        Some(handle & !(u32::MAX as u64) | offset)
    }
}

impl CpuDescriptorHandle {
    /// The handle `index` increments past this one, if it stays in its heap's range.
    pub const fn offset(self, index: u32) -> Option<Self> {
        match offset_handle(self.0, index) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl GpuDescriptorHandle {
    /// The handle `index` increments past this one, if it stays in its heap's range.
    pub const fn offset(self, index: u32) -> Option<Self> {
        match offset_handle(self.0, index) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Texel format of a texture a shader resource view reads. Rows are linear
/// and tightly packed: four bytes per texel, or eight bytes per 4×4 block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureFormat {
    Rgba8Unorm,
    Bc1Unorm,
}

impl TextureFormat {
    /// The format for a DXGI format code, `R8G8B8A8_UNORM` (28) or `BC1_UNORM` (71).
    pub const fn from_dxgi(format: u32) -> Option<Self> {
        match format {
            DXGI_FORMAT_R8G8B8A8_UNORM => Some(Self::Rgba8Unorm),
            DXGI_FORMAT_BC1_UNORM => Some(Self::Bc1Unorm),
            _ => None,
        }
    }

    /// Bytes of one level of `width` × `height` texels.
    pub const fn level_size(self, width: u32, height: u32) -> usize {
        match self {
            Self::Rgba8Unorm => width as usize * height as usize * 4,
            Self::Bc1Unorm => width.div_ceil(4) as usize * height.div_ceil(4) as usize * 8,
        }
    }
}

/// `D3D12_SRV_DIMENSION`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SrvDimension {
    Buffer,
    Texture1D,
    Texture2D,
    Texture2DArray,
    Texture3D,
    TextureCube,
}

/// `D3D12_SHADER_RESOURCE_VIEW_DESC` for a single-level 2D texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShaderResourceViewDesc {
    /// DXGI format code; it must equal the resource's format.
    pub format: u32,
    pub view_dimension: SrvDimension,
    pub most_detailed_mip: u32,
    pub mip_levels: u32,
}

/// `D3D12_SAMPLER_DESC` without the fields that point sampling ignores.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SamplerDesc {
    pub filter: u32,
    pub address_u: u32,
    pub address_v: u32,
    pub address_w: u32,
    pub mip_lod_bias: f32,
    pub min_lod: f32,
    pub max_lod: f32,
}

impl SamplerDesc {
    /// Point filtering with clamp addressing, the one supported state.
    pub const fn point_clamp() -> Self {
        Self {
            filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
            address_u: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            address_v: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            address_w: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            mip_lod_bias: 0.0,
            min_lod: 0.0,
            max_lod: f32::MAX,
        }
    }

    fn validate(&self) -> Result<(), GraphicsError> {
        if self.filter != D3D12_FILTER_MIN_MAG_MIP_POINT {
            return Err(GraphicsError::Unsupported(
                "sampler filter other than point",
            ));
        }
        if [self.address_u, self.address_v, self.address_w]
            .iter()
            .any(|&mode| mode != D3D12_TEXTURE_ADDRESS_MODE_CLAMP)
        {
            return Err(GraphicsError::Unsupported(
                "sampler address mode other than clamp",
            ));
        }
        if self.mip_lod_bias != 0.0
            || self.min_lod != 0.0
            || self.max_lod.is_nan()
            || self.max_lod < 0.0
        {
            return Err(GraphicsError::Unsupported("sampler LOD bias or range"));
        }
        Ok(())
    }
}

/// `D3D12_DESCRIPTOR_RANGE_TYPE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorRangeType {
    Srv,
    Uav,
    Cbv,
    Sampler,
}

/// `D3D12_DESCRIPTOR_RANGE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescriptorRange {
    pub range_type: DescriptorRangeType,
    pub num_descriptors: u32,
    pub base_shader_register: u32,
    pub register_space: u32,
    pub offset_in_descriptors_from_table_start: u32,
}

/// `D3D12_SHADER_VISIBILITY`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderVisibility {
    All,
    Vertex,
    Hull,
    Domain,
    Geometry,
    Pixel,
}

/// `D3D12_ROOT_PARAMETER`, by parameter type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootParameter {
    DescriptorTable {
        ranges: Vec<DescriptorRange>,
        visibility: ShaderVisibility,
    },
    Constants32Bit {
        visibility: ShaderVisibility,
    },
    Cbv {
        visibility: ShaderVisibility,
    },
    Srv {
        visibility: ShaderVisibility,
    },
    Uav {
        visibility: ShaderVisibility,
    },
}

/// `D3D12_ROOT_SIGNATURE_DESC`, unserialized. Static samplers are counted only:
/// any is unsupported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RootSignatureDesc {
    pub parameters: Vec<RootParameter>,
    pub static_sampler_count: u32,
}

/// The supported root signature: one pixel-stage table holding `t0` and
/// one holding `s0`, at these root parameter indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RootSignature {
    /// The device that created this root signature.
    device: GraphicsHandle,
    srv_parameter: u32,
    sampler_parameter: u32,
}

impl RootSignature {
    fn from_desc(device: GraphicsHandle, desc: &RootSignatureDesc) -> Result<Self, GraphicsError> {
        if desc.static_sampler_count != 0 {
            return Err(GraphicsError::Unsupported("static samplers"));
        }
        if desc.parameters.len() != 2 {
            return Err(GraphicsError::Unsupported(
                "root signature other than one SRV table and one sampler table",
            ));
        }
        let mut srv = None;
        let mut sampler = None;
        for (index, parameter) in (0u32..).zip(&desc.parameters) {
            let RootParameter::DescriptorTable { ranges, visibility } = parameter else {
                return Err(GraphicsError::Unsupported(
                    "root constants or root descriptors",
                ));
            };
            if *visibility != ShaderVisibility::Pixel {
                return Err(GraphicsError::Unsupported("shader stage other than pixel"));
            }
            let [range] = ranges.as_slice() else {
                return Err(GraphicsError::Unsupported(
                    "descriptor table with other than one range",
                ));
            };
            if range.num_descriptors != 1
                || range.base_shader_register != 0
                || range.register_space != 0
                || !matches!(
                    range.offset_in_descriptors_from_table_start,
                    0 | DESCRIPTOR_RANGE_OFFSET_APPEND
                )
            {
                return Err(GraphicsError::Unsupported(
                    "descriptor range other than one descriptor at register 0, space 0",
                ));
            }
            let slot = match range.range_type {
                DescriptorRangeType::Srv => &mut srv,
                DescriptorRangeType::Sampler => &mut sampler,
                DescriptorRangeType::Uav => return Err(GraphicsError::Unsupported("UAV range")),
                DescriptorRangeType::Cbv => return Err(GraphicsError::Unsupported("CBV range")),
            };
            if slot.replace(index).is_some() {
                return Err(GraphicsError::Unsupported(
                    "more than one table of one range type",
                ));
            }
        }
        match (srv, sampler) {
            (Some(srv_parameter), Some(sampler_parameter)) => Ok(Self {
                device,
                srv_parameter,
                sampler_parameter,
            }),
            _ => Err(GraphicsError::Unsupported(
                "root signature without both an SRV and a sampler table",
            )),
        }
    }
}

/// One filled descriptor.
#[derive(Clone, Copy, Debug)]
enum Descriptor {
    Srv {
        resource: GraphicsHandle,
        format: TextureFormat,
    },
    Sampler(SamplerDesc),
}

/// Per-execution root signature, heap and table bindings.
#[derive(Default)]
struct TableBindings {
    root_signature: Option<(GraphicsHandle, RootSignature)>,
    srv_heap: Option<u32>,
    sampler_heap: Option<u32>,
    tables: [Option<GpuDescriptorHandle>; 2],
}

/// A shader-visible descriptor heap.
struct DescriptorHeap {
    handle: GraphicsHandle,
    /// The device that created this heap.
    device: GraphicsHandle,
    heap_type: DescriptorHeapType,
    slots: Vec<Option<Descriptor>>,
}

/// The texture a textured draw samples, as bound through its SRV table.
#[derive(Clone, Copy, Debug)]
pub struct TextureView<'a> {
    pub format: TextureFormat,
    pub width: u32,
    pub height: u32,
    /// The single level: tightly packed rows of texels or BC1 blocks.
    pub bytes: &'a [u8],
}

/// One triangle that samples a bound texture in the pixel stage.
///
/// Each vertex supplies a float4 position and a float4 texture coordinate,
/// whose `xy` are the normalized `uv` the pixel stage samples at.
#[derive(Clone, Copy, Debug)]
pub struct TexturedDraw<'a> {
    pub positions: [[f32; 4]; 3],
    pub texcoords: [[f32; 4]; 3],
    pub viewport: (usize, usize),
    pub clear_color: [u8; 4],
    pub texture: TextureView<'a>,
    pub sampler: SamplerDesc,
}

/// Backend renderer for textured draws.
pub type TexturedBackendRenderer<'a> =
    dyn Fn(&TexturedDraw<'_>) -> Result<Vec<u8>, GraphicsError> + 'a;

/// Shared thread-safe textured backend renderer instance.
pub type SharedTexturedBackendRenderer =
    Arc<dyn Fn(&TexturedDraw<'_>) -> Result<Vec<u8>, GraphicsError> + Send + Sync>;

/// Primitive Draw Command recorded in a Command List.
#[derive(Clone, Debug)]
pub enum GraphicsCommand {
    ResourceBarrier {
        resource: GraphicsHandle,
        before: ResourceStates,
        after: ResourceStates,
    },
    SetRenderTargets {
        color_target: Option<GraphicsHandle>,
        clear_color: [u8; 4],
    },
    SetViewport {
        width: usize,
        height: usize,
    },
    SetVertexBuffer {
        resource: GraphicsHandle,
        offset: usize,
        stride: usize,
    },
    DrawInstanced {
        vertex_count_per_instance: u32,
        instance_count: u32,
        start_vertex_location: u32,
        start_instance_location: u32,
    },
    SetIndexBuffer {
        resource: GraphicsHandle,
        offset: usize,
        format: IndexFormat,
    },
    DrawIndexedInstanced {
        index_count_per_instance: u32,
        instance_count: u32,
        start_index_location: u32,
        base_vertex_location: i32,
        start_instance_location: u32,
    },
    /// `SetDescriptorHeaps`: at most one heap of each type.
    SetDescriptorHeaps {
        heaps: Vec<GraphicsHandle>,
    },
    /// `SetGraphicsRootSignature`; it clears every bound table.
    SetGraphicsRootSignature {
        root_signature: GraphicsHandle,
    },
    /// `SetGraphicsRootDescriptorTable`.
    SetGraphicsRootDescriptorTable {
        root_parameter_index: u32,
        base: GpuDescriptorHandle,
    },
}

/// Command List in recording or closed state.
pub struct GraphicsCommandList {
    pub handle: GraphicsHandle,
    /// The device that created this command list.
    pub device: GraphicsHandle,
    /// The command allocator this list was created from.
    pub allocator: GraphicsHandle,
    pub list_type: CommandListType,
    pub is_recording: bool,
    pub commands: Vec<GraphicsCommand>,
}

/// Command Allocator tracking command recording buffers.
pub struct GraphicsCommandAllocator {
    pub handle: GraphicsHandle,
    /// The device that created this allocator.
    pub device: GraphicsHandle,
    pub list_type: CommandListType,
    pub is_reset: bool,
}

/// Command Queue for command list submission and execution.
pub struct GraphicsCommandQueue {
    pub handle: GraphicsHandle,
    /// The device that created this queue.
    pub device: GraphicsHandle,
    pub list_type: CommandListType,
    pub submitted_lists: Vec<GraphicsHandle>,
    fence_waits: Vec<(GraphicsHandle, u64)>,
}

/// A native fence operation. The adapter returns the completed value it
/// observed. Queue signals and waits are encoded on the GPU. A host wait
/// that reaches its deadline returns the last observed value.
#[derive(Clone, Copy, Debug)]
pub enum FenceOperation {
    CompletedValue,
    Signal(u64),
    QueueWait { value: u64, timeout: Duration },
    HostWait { value: u64, timeout: Duration },
}

pub type NativeFence = Box<dyn Fn(FenceOperation) -> Result<u64, GraphicsError> + Send + Sync>;
type NativeFenceFactory = Box<dyn Fn(u64) -> Result<NativeFence, GraphicsError> + Send + Sync>;

struct GraphicsFence {
    device: GraphicsHandle,
    // The software model executes serially, so a submitted signal has already
    // completed. The native adapter reads its own shared event.
    last_signal: u64,
    native: Option<NativeFence>,
}

impl GraphicsFence {
    fn completed_value(&self) -> Result<u64, GraphicsError> {
        match &self.native {
            Some(native) => native(FenceOperation::CompletedValue),
            None => Ok(self.last_signal),
        }
    }
}

/// Custom backend renderer signature for testing or Metal dispatch.
pub type BackendRenderer<'a> =
    dyn Fn(&[[f32; 4]; 3], (usize, usize), [u8; 4]) -> Result<Vec<u8>, GraphicsError> + 'a;

/// Shared thread-safe backend renderer instance.
pub type SharedBackendRenderer = Arc<
    dyn Fn(&[[f32; 4]; 3], (usize, usize), [u8; 4]) -> Result<Vec<u8>, GraphicsError> + Send + Sync,
>;

/// Frame presentation capture holding the last presented frame.
#[derive(Clone, Debug, Default)]
pub struct FramePresentation {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
    pub frame_index: u64,
    /// The number of draws the last executed command list performed.
    pub draw_count: usize,
    pub backend: Option<GraphicsBackendId>,
}

/// Central Graphics Context maintaining guest-visible objects and mapping them to Metal.
pub struct GraphicsContext {
    next_id: u32,
    devices: HashMap<GraphicsHandle, bool>,
    queues: HashMap<GraphicsHandle, GraphicsCommandQueue>,
    fences: HashMap<GraphicsHandle, GraphicsFence>,
    native_fences: Option<NativeFenceFactory>,
    allocators: HashMap<GraphicsHandle, GraphicsCommandAllocator>,
    command_lists: HashMap<GraphicsHandle, GraphicsCommandList>,
    resources: HashMap<GraphicsHandle, GraphicsResource>,
    /// Descriptor heaps by handle ID, which descriptor handles encode.
    descriptor_heaps: HashMap<u32, DescriptorHeap>,
    root_signatures: HashMap<GraphicsHandle, RootSignature>,
    pub last_presentation: Arc<Mutex<FramePresentation>>,
    /// Optional active backend renderer (e.g. Native Metal).
    backend: Option<SharedBackendRenderer>,
    /// Optional backend renderer for textured draws.
    textured_backend: Option<SharedTexturedBackendRenderer>,
    backend_id: GraphicsBackendId,
}

impl Default for GraphicsContext {
    fn default() -> Self {
        Self::new()
    }
}

impl GraphicsContext {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            devices: HashMap::new(),
            queues: HashMap::new(),
            fences: HashMap::new(),
            native_fences: None,
            allocators: HashMap::new(),
            command_lists: HashMap::new(),
            resources: HashMap::new(),
            descriptor_heaps: HashMap::new(),
            root_signatures: HashMap::new(),
            last_presentation: Arc::new(Mutex::new(FramePresentation::default())),
            backend: None,
            textured_backend: None,
            backend_id: GraphicsBackendId::SoftwareReference,
        }
    }

    /// Install a native Metal backend adapter.
    /// Live object counts for the service tests: devices, queues, allocators,
    /// command lists and resources. A creation that fails must not change them.
    #[cfg(test)]
    pub(crate) fn live_counts(&self) -> [usize; 5] {
        [
            self.devices.len(),
            self.queues.len(),
            self.allocators.len(),
            self.command_lists.len(),
            self.resources.len(),
        ]
    }

    pub fn set_metal_backend<F>(&mut self, renderer: F)
    where
        F: Fn(&[[f32; 4]; 3], (usize, usize), [u8; 4]) -> Result<Vec<u8>, GraphicsError>
            + Send
            + Sync
            + 'static,
    {
        self.backend = Some(Arc::new(renderer));
        self.backend_id = GraphicsBackendId::MetalNative;
    }

    /// Install a native Metal adapter for draws that sample a bound texture.
    pub fn set_metal_textured_backend<F>(&mut self, renderer: F)
    where
        F: Fn(&TexturedDraw<'_>) -> Result<Vec<u8>, GraphicsError> + Send + Sync + 'static,
    {
        self.textured_backend = Some(Arc::new(renderer));
        self.backend_id = GraphicsBackendId::MetalNative;
    }

    /// Explicitly configure context to require native Metal without an attached backend.
    /// Useful for verifying that missing/unsupported Metal devices correctly fail execution.
    pub fn set_require_metal_without_backend(&mut self) {
        self.backend = None;
        self.textured_backend = None;
        self.backend_id = GraphicsBackendId::MetalNative;
    }

    /// Retrieve the currently configured backend ID.
    pub fn current_backend_id(&self) -> GraphicsBackendId {
        self.backend_id
    }

    fn allocate_handle(&mut self) -> GraphicsHandle {
        let id = self.next_id;
        self.next_id += 1;
        GraphicsHandle { id, generation: 1 }
    }

    /// Create guest graphics device (`D3D12CreateDevice` / test device).
    pub fn create_device(&mut self) -> Result<GraphicsHandle, GraphicsError> {
        let handle = self.allocate_handle();
        self.devices.insert(handle, true);
        Ok(handle)
    }

    /// Create command queue (`ID3D12Device::CreateCommandQueue`).
    pub fn create_command_queue(
        &mut self,
        device: GraphicsHandle,
        list_type: CommandListType,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let handle = self.allocate_handle();
        self.queues.insert(
            handle,
            GraphicsCommandQueue {
                handle,
                device,
                list_type,
                submitted_lists: Vec::new(),
                fence_waits: Vec::new(),
            },
        );
        Ok(handle)
    }

    /// Install the shared-event factory for the configured native renderer.
    /// A context cannot switch fence backends while it owns live fences.
    pub fn set_native_fence_factory(
        &mut self,
        factory: impl Fn(u64) -> Result<NativeFence, GraphicsError> + Send + Sync + 'static,
    ) -> Result<(), GraphicsError> {
        if !self.fences.is_empty() {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.native_fences = Some(Box::new(factory));
        Ok(())
    }

    /// Create a device-owned timeline fence at the supplied initial value.
    pub fn create_fence(
        &mut self,
        device: GraphicsHandle,
        initial_value: u64,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        if self.fences.len() >= MAX_FENCES {
            return Err(GraphicsError::FenceLimitExceeded);
        }
        // D3D12 uses UINT64_MAX to report device removal.
        if initial_value == u64::MAX {
            return Err(GraphicsError::Unsupported("reserved fence value"));
        }
        let native = match self.backend_id {
            GraphicsBackendId::SoftwareReference => None,
            GraphicsBackendId::MetalNative => {
                let factory = self
                    .native_fences
                    .as_ref()
                    .ok_or(GraphicsError::NativeMetalRequired)?;
                Some(factory(initial_value)?)
            }
        };
        let handle = self.allocate_handle();
        self.fences.insert(
            handle,
            GraphicsFence {
                device,
                last_signal: initial_value,
                native,
            },
        );
        Ok(handle)
    }

    pub fn destroy_fence(&mut self, fence: GraphicsHandle) -> Result<(), GraphicsError> {
        self.fence(fence)?;
        if self
            .queues
            .values()
            .any(|queue| queue.fence_waits.iter().any(|(handle, _)| *handle == fence))
        {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.fences.remove(&fence);
        Ok(())
    }

    pub fn fence_completed_value(&self, fence: GraphicsHandle) -> Result<u64, GraphicsError> {
        self.fence(fence)?.completed_value()
    }

    /// Block the host for at most the caller's budget (up to one second).
    /// Zero is a poll. Times beyond the bound are explicitly unsupported.
    pub fn wait_for_fence(
        &self,
        fence: GraphicsHandle,
        value: u64,
        timeout: Duration,
    ) -> Result<(), GraphicsError> {
        let fence = self.fence(fence)?;
        if timeout > MAX_FENCE_WAIT || value == u64::MAX {
            return Err(GraphicsError::Unsupported("fence wait value or timeout"));
        }
        let completed = fence.completed_value()?;
        if completed >= value || timeout.is_zero() {
            return Self::check_fence_value(value, completed);
        }
        let reached = match &fence.native {
            Some(native) => native(FenceOperation::HostWait { value, timeout })?,
            None => {
                // No other thread can submit work to this context during a
                // host wait in the deterministic reference implementation.
                std::thread::sleep(timeout);
                completed
            }
        };
        Self::check_fence_value(value, reached)
    }

    /// Record an ordered dependency and return without waiting on the host.
    /// In this synchronous graphics path the next execute or signal applies
    /// the dependency. A not-yet-submitted signal makes that operation
    /// retryable; the queue retains its dependency in that case.
    pub fn queue_wait(
        &mut self,
        queue: GraphicsHandle,
        fence: GraphicsHandle,
        value: u64,
    ) -> Result<(), GraphicsError> {
        self.check_queue_fence(queue, fence)?;
        if value == u64::MAX {
            return Err(GraphicsError::Unsupported("reserved fence value"));
        }
        let waits = &mut self.queues.get_mut(&queue).unwrap().fence_waits;
        if let Some((_, wanted)) = waits.iter_mut().find(|(handle, _)| *handle == fence) {
            *wanted = (*wanted).max(value);
        } else {
            if waits.len() >= MAX_QUEUE_WAITS {
                return Err(GraphicsError::FenceLimitExceeded);
            }
            waits.push((fence, value));
        }
        Ok(())
    }

    /// Signal after earlier work on this queue. The existing render backend
    /// returns only after its draw and readback complete.
    pub fn queue_signal(
        &mut self,
        queue: GraphicsHandle,
        fence: GraphicsHandle,
        value: u64,
    ) -> Result<(), GraphicsError> {
        self.check_queue_fence(queue, fence)?;
        let timeline = self.fence(fence)?;
        if value == u64::MAX || value < timeline.last_signal {
            return Err(GraphicsError::Unsupported(
                "reserved or decreasing fence signal",
            ));
        }
        self.apply_queue_waits(queue, self.backend_id)?;
        let timeline = self.fences.get_mut(&fence).unwrap();
        if let Some(native) = &timeline.native {
            native(FenceOperation::Signal(value))?;
        }
        timeline.last_signal = value;
        Ok(())
    }

    fn fence(&self, fence: GraphicsHandle) -> Result<&GraphicsFence, GraphicsError> {
        self.fences
            .get(&fence)
            .ok_or(GraphicsError::InvalidFenceHandle)
    }

    fn check_queue_fence(
        &self,
        queue: GraphicsHandle,
        fence: GraphicsHandle,
    ) -> Result<(), GraphicsError> {
        let queue = self
            .queues
            .get(&queue)
            .ok_or(GraphicsError::InvalidQueueHandle)?;
        let fence = self.fence(fence)?;
        if queue.device != fence.device {
            return Err(GraphicsError::Unsupported(
                "fence belongs to another device",
            ));
        }
        if fence.native.is_some() != (self.backend_id == GraphicsBackendId::MetalNative) {
            return Err(GraphicsError::Unsupported(
                "fence belongs to another backend",
            ));
        }
        Ok(())
    }

    fn check_fence_value(value: u64, completed: u64) -> Result<(), GraphicsError> {
        if completed < value {
            return Err(GraphicsError::FenceTimeout {
                requested: value,
                completed,
            });
        }
        Ok(())
    }

    fn apply_queue_waits(
        &mut self,
        queue: GraphicsHandle,
        backend: GraphicsBackendId,
    ) -> Result<(), GraphicsError> {
        let waits = &self
            .queues
            .get(&queue)
            .ok_or(GraphicsError::InvalidQueueHandle)?
            .fence_waits;
        // Check all prerequisites before submitting any command. In
        // particular, do not enqueue a Metal wait whose producer is missing.
        for &(handle, value) in waits {
            let fence = self.fence(handle)?;
            if fence.native.is_some() != (backend == GraphicsBackendId::MetalNative) {
                return Err(GraphicsError::Unsupported(
                    "fence belongs to another backend",
                ));
            }
            if fence.last_signal < value {
                return Err(GraphicsError::FenceTimeout {
                    requested: value,
                    completed: fence.completed_value()?,
                });
            }
        }
        let started = Instant::now();
        for &(handle, value) in waits {
            let fence = self.fence(handle)?;
            if let Some(native) = &fence.native {
                let timeout = MAX_FENCE_WAIT.saturating_sub(started.elapsed());
                let completed = native(FenceOperation::QueueWait { value, timeout })?;
                Self::check_fence_value(value, completed)?;
            }
        }
        self.queues.get_mut(&queue).unwrap().fence_waits.clear();
        Ok(())
    }

    /// Create command allocator (`ID3D12Device::CreateCommandAllocator`).
    pub fn create_command_allocator(
        &mut self,
        device: GraphicsHandle,
        list_type: CommandListType,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let handle = self.allocate_handle();
        self.allocators.insert(
            handle,
            GraphicsCommandAllocator {
                handle,
                device,
                list_type,
                is_reset: true,
            },
        );
        Ok(handle)
    }

    /// Create command list (`ID3D12Device::CreateCommandList`).
    pub fn create_command_list(
        &mut self,
        device: GraphicsHandle,
        allocator: GraphicsHandle,
        list_type: CommandListType,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let alloc = self
            .allocators
            .get(&allocator)
            .ok_or(GraphicsError::InvalidAllocatorHandle)?;
        if alloc.list_type != list_type {
            return Err(GraphicsError::InvalidAllocatorHandle);
        }

        let handle = self.allocate_handle();
        self.command_lists.insert(
            handle,
            GraphicsCommandList {
                handle,
                device,
                allocator,
                list_type,
                is_recording: true,
                commands: Vec::new(),
            },
        );
        Ok(handle)
    }

    /// Create committed resource (Buffer or Texture).
    pub fn create_committed_resource(
        &mut self,
        device: GraphicsHandle,
        desc: ResourceDesc,
        initial_state: ResourceStates,
        guest_addr: Option<u64>,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let handle = self.allocate_handle();
        let texture_format = (desc.dimension == ResourceDimension::Texture2D && desc.format_rgba8)
            .then_some(TextureFormat::Rgba8Unorm);
        let size = match desc.dimension {
            ResourceDimension::Buffer => desc.width as usize,
            ResourceDimension::Texture2D => (desc.width as usize) * (desc.height as usize) * 4,
            _ => desc.width as usize,
        };
        self.resources.insert(
            handle,
            GraphicsResource {
                handle,
                device,
                desc,
                state: initial_state,
                guest_backing_addr: guest_addr,
                data: vec![0u8; size],
                texture_format,
            },
        );
        Ok(handle)
    }

    /// Record resource barrier in command list.
    pub fn cmd_resource_barrier(
        &mut self,
        list: GraphicsHandle,
        resource: GraphicsHandle,
        before: ResourceStates,
        after: ResourceStates,
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        if !self.resources.contains_key(&resource) {
            return Err(GraphicsError::InvalidResourceHandle);
        }
        cmd_list.commands.push(GraphicsCommand::ResourceBarrier {
            resource,
            before,
            after,
        });
        Ok(())
    }

    /// Record set render targets.
    pub fn cmd_set_render_targets(
        &mut self,
        list: GraphicsHandle,
        color_target: Option<GraphicsHandle>,
        clear_color: [u8; 4],
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        if let Some(target) = color_target
            && !self.resources.contains_key(&target)
        {
            return Err(GraphicsError::InvalidResourceHandle);
        }
        cmd_list.commands.push(GraphicsCommand::SetRenderTargets {
            color_target,
            clear_color,
        });
        Ok(())
    }

    /// Record set viewport.
    pub fn cmd_set_viewport(
        &mut self,
        list: GraphicsHandle,
        width: usize,
        height: usize,
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        if width == 0 || height == 0 || width > 4096 || height > 4096 {
            return Err(GraphicsError::InvalidViewport);
        }
        cmd_list
            .commands
            .push(GraphicsCommand::SetViewport { width, height });
        Ok(())
    }

    /// Record a vertex-buffer binding backed by a buffer resource.
    pub fn cmd_set_vertex_buffer(
        &mut self,
        list: GraphicsHandle,
        resource: GraphicsHandle,
        offset: usize,
        stride: usize,
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        let res = self
            .resources
            .get(&resource)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        if res.desc.dimension != ResourceDimension::Buffer {
            return Err(GraphicsError::InvalidDescriptor);
        }
        let end = offset
            .checked_add(stride)
            .ok_or(GraphicsError::InvalidDescriptor)?;
        if end > res.data.len() && res.guest_backing_addr.is_none() {
            return Err(GraphicsError::InvalidDescriptor);
        }
        cmd_list.commands.push(GraphicsCommand::SetVertexBuffer {
            resource,
            offset,
            stride,
        });
        Ok(())
    }

    /// Record draw instanced.
    pub fn cmd_draw_instanced(
        &mut self,
        list: GraphicsHandle,
        vertex_count_per_instance: u32,
        instance_count: u32,
        start_vertex_location: u32,
        start_instance_location: u32,
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        cmd_list.commands.push(GraphicsCommand::DrawInstanced {
            vertex_count_per_instance,
            instance_count,
            start_vertex_location,
            start_instance_location,
        });
        Ok(())
    }

    /// Record an index-buffer binding backed by a buffer resource.
    /// `format` is a DXGI format code; only
    /// `R16_UINT` (57) and `R32_UINT` (42) are index formats.
    pub fn cmd_set_index_buffer(
        &mut self,
        list: GraphicsHandle,
        resource: GraphicsHandle,
        offset: usize,
        format: u32,
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        let format =
            IndexFormat::from_dxgi(format).ok_or(GraphicsError::InvalidIndexFormat(format))?;
        let res = self
            .resources
            .get(&resource)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        if res.desc.dimension != ResourceDimension::Buffer
            || !offset.is_multiple_of(format.size())
            || offset > res.data.len()
        {
            return Err(GraphicsError::InvalidDescriptor);
        }
        cmd_list.commands.push(GraphicsCommand::SetIndexBuffer {
            resource,
            offset,
            format,
        });
        Ok(())
    }

    /// Record draw indexed instanced. Each draw renders one triangle once: at
    /// execution the index count must be 0 or 3, the instance count 0 or 1 and
    /// the start instance 0, or the draw is rejected; a zero index or instance
    /// count draws nothing. Index values are ordinary triangle-list indices
    /// (no primitive restart), range checked after adding the base vertex.
    /// With a textured root signature, both position and texture coordinates
    /// come from each resolved vertex.
    pub fn cmd_draw_indexed_instanced(
        &mut self,
        list: GraphicsHandle,
        index_count_per_instance: u32,
        instance_count: u32,
        start_index_location: u32,
        base_vertex_location: i32,
        start_instance_location: u32,
    ) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        cmd_list
            .commands
            .push(GraphicsCommand::DrawIndexedInstanced {
                index_count_per_instance,
                instance_count,
                start_index_location,
                base_vertex_location,
                start_instance_location,
            });
        Ok(())
    }

    /// Create a committed single-level 2D texture (`CreateCommittedResource`
    /// with a texture format). `format` is a DXGI code: `R8G8B8A8_UNORM` (28)
    /// or `BC1_UNORM` (71). BC1 sizes must be multiples of four.
    pub fn create_committed_texture2d(
        &mut self,
        device: GraphicsHandle,
        width: u32,
        height: u32,
        format: u32,
        initial_state: ResourceStates,
        guest_addr: Option<u64>,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let format =
            TextureFormat::from_dxgi(format).ok_or(GraphicsError::Unsupported("texture format"))?;
        if !(1..=MAX_TEXTURE_DIMENSION).contains(&width)
            || !(1..=MAX_TEXTURE_DIMENSION).contains(&height)
        {
            return Err(GraphicsError::Unsupported("texture size"));
        }
        if format == TextureFormat::Bc1Unorm
            && (!width.is_multiple_of(4) || !height.is_multiple_of(4))
        {
            return Err(GraphicsError::Unsupported(
                "BC1 size not a multiple of four",
            ));
        }
        let handle = self.allocate_handle();
        self.resources.insert(
            handle,
            GraphicsResource {
                handle,
                device,
                desc: ResourceDesc {
                    dimension: ResourceDimension::Texture2D,
                    width: u64::from(width),
                    height,
                    depth_or_array_size: 1,
                    mip_levels: 1,
                    format_rgba8: format == TextureFormat::Rgba8Unorm,
                },
                state: initial_state,
                guest_backing_addr: guest_addr,
                data: vec![0; format.level_size(width, height)],
                texture_format: Some(format),
            },
        );
        Ok(handle)
    }

    /// Create a descriptor heap (`CreateDescriptorHeap`). `heap_type` is a
    /// `D3D12_DESCRIPTOR_HEAP_TYPE`; only shader-visible `CBV_SRV_UAV` and
    /// `SAMPLER` heaps of 1 to [`MAX_DESCRIPTORS_PER_HEAP`] descriptors exist.
    pub fn create_descriptor_heap(
        &mut self,
        device: GraphicsHandle,
        heap_type: u32,
        num_descriptors: u32,
        shader_visible: bool,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let heap_type = DescriptorHeapType::from_d3d12(heap_type)
            .ok_or(GraphicsError::Unsupported("descriptor heap type"))?;
        if !shader_visible {
            return Err(GraphicsError::Unsupported(
                "non-shader-visible descriptor heap",
            ));
        }
        if !(1..=MAX_DESCRIPTORS_PER_HEAP).contains(&num_descriptors) {
            return Err(GraphicsError::Unsupported("descriptor heap capacity"));
        }
        let handle = self.allocate_handle();
        if handle.id >= 1 << 31 {
            return Err(GraphicsError::Unsupported(
                "descriptor heap ID space exhausted",
            ));
        }
        self.descriptor_heaps.insert(
            handle.id,
            DescriptorHeap {
                handle,
                device,
                heap_type,
                slots: vec![None; num_descriptors as usize],
            },
        );
        Ok(handle)
    }

    fn descriptor_heap(&self, heap: GraphicsHandle) -> Result<&DescriptorHeap, GraphicsError> {
        self.descriptor_heaps
            .get(&heap.id)
            .filter(|found| found.handle == heap)
            .ok_or(GraphicsError::InvalidDescriptor)
    }

    /// `GetCPUDescriptorHandleForHeapStart`.
    pub fn cpu_descriptor_handle_for_heap_start(
        &self,
        heap: GraphicsHandle,
    ) -> Result<CpuDescriptorHandle, GraphicsError> {
        self.descriptor_heap(heap)?;
        Ok(CpuDescriptorHandle(u64::from(heap.id) << 32))
    }

    /// `GetGPUDescriptorHandleForHeapStart`.
    pub fn gpu_descriptor_handle_for_heap_start(
        &self,
        heap: GraphicsHandle,
    ) -> Result<GpuDescriptorHandle, GraphicsError> {
        self.descriptor_heap(heap)?;
        Ok(GpuDescriptorHandle(
            GPU_DESCRIPTOR_SPACE | u64::from(heap.id) << 32,
        ))
    }

    /// `GetDescriptorHandleIncrementSize` for a `D3D12_DESCRIPTOR_HEAP_TYPE`.
    pub fn descriptor_handle_increment_size(&self, heap_type: u32) -> Result<u32, GraphicsError> {
        DescriptorHeapType::from_d3d12(heap_type)
            .map(|_| DESCRIPTOR_HANDLE_INCREMENT)
            .ok_or(GraphicsError::Unsupported("descriptor heap type"))
    }

    /// The heap ID and slot index a handle in the given space names, if it is
    /// increment-aligned. The heap itself is not checked.
    fn decode_descriptor_handle(value: u64, gpu: bool) -> Option<(u32, usize)> {
        if (value & GPU_DESCRIPTOR_SPACE != 0) != gpu {
            return None;
        }
        let value = value & !GPU_DESCRIPTOR_SPACE;
        let offset = value as u32;
        if !offset.is_multiple_of(DESCRIPTOR_HANDLE_INCREMENT) {
            return None;
        }
        Some((
            (value >> 32) as u32,
            (offset / DESCRIPTOR_HANDLE_INCREMENT) as usize,
        ))
    }

    fn write_descriptor(
        &mut self,
        dest: CpuDescriptorHandle,
        heap_type: DescriptorHeapType,
        descriptor: Descriptor,
    ) -> Result<(), GraphicsError> {
        let (heap_id, index) = Self::decode_descriptor_handle(dest.0, false)
            .ok_or(GraphicsError::InvalidDescriptor)?;
        let heap = self
            .descriptor_heaps
            .get_mut(&heap_id)
            .ok_or(GraphicsError::InvalidDescriptor)?;
        if heap.heap_type != heap_type {
            return Err(GraphicsError::InvalidDescriptor);
        }
        let slot = heap
            .slots
            .get_mut(index)
            .ok_or(GraphicsError::InvalidDescriptor)?;
        *slot = Some(descriptor);
        Ok(())
    }

    /// `CreateShaderResourceView` of a single-level 2D texture into a
    /// `CBV_SRV_UAV` heap slot. The view format must equal the resource's.
    pub fn create_shader_resource_view(
        &mut self,
        resource: GraphicsHandle,
        desc: &ShaderResourceViewDesc,
        dest: CpuDescriptorHandle,
    ) -> Result<(), GraphicsError> {
        let res = self
            .resources
            .get(&resource)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        if res.desc.dimension != ResourceDimension::Texture2D
            || res.desc.depth_or_array_size != 1
            || res.desc.mip_levels != 1
        {
            return Err(GraphicsError::Unsupported(
                "SRV of a resource other than a single-level, single-slice 2D texture",
            ));
        }
        if desc.view_dimension != SrvDimension::Texture2D {
            return Err(GraphicsError::Unsupported(
                "SRV dimension other than Texture2D",
            ));
        }
        if desc.most_detailed_mip != 0 || desc.mip_levels != 1 {
            return Err(GraphicsError::Unsupported(
                "SRV mip range other than level 0",
            ));
        }
        let format = TextureFormat::from_dxgi(desc.format)
            .ok_or(GraphicsError::Unsupported("SRV format"))?;
        if res.texture_format != Some(format) {
            return Err(GraphicsError::Unsupported(
                "SRV format differs from the resource",
            ));
        }
        let max = u64::from(MAX_TEXTURE_DIMENSION);
        if !(1..=max).contains(&res.desc.width) || !(1..=max).contains(&u64::from(res.desc.height))
        {
            return Err(GraphicsError::Unsupported("texture size"));
        }
        self.write_descriptor(
            dest,
            DescriptorHeapType::CbvSrvUav,
            Descriptor::Srv { resource, format },
        )
    }

    /// `CreateSampler` into a `SAMPLER` heap slot.
    pub fn create_sampler(
        &mut self,
        desc: &SamplerDesc,
        dest: CpuDescriptorHandle,
    ) -> Result<(), GraphicsError> {
        desc.validate()?;
        self.write_descriptor(
            dest,
            DescriptorHeapType::Sampler,
            Descriptor::Sampler(*desc),
        )
    }

    /// `CreateRootSignature` from an unserialized description. Only one
    /// pixel-stage table for `t0` and one for `s0` are supported.
    pub fn create_root_signature(
        &mut self,
        device: GraphicsHandle,
        desc: &RootSignatureDesc,
    ) -> Result<GraphicsHandle, GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let root_signature = RootSignature::from_desc(device, desc)?;
        let handle = self.allocate_handle();
        self.root_signatures.insert(handle, root_signature);
        Ok(handle)
    }

    fn recording_list(
        &mut self,
        list: GraphicsHandle,
    ) -> Result<&mut GraphicsCommandList, GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        Ok(cmd_list)
    }

    /// Record `SetDescriptorHeaps`: at most one heap of each type.
    pub fn cmd_set_descriptor_heaps(
        &mut self,
        list: GraphicsHandle,
        heaps: &[GraphicsHandle],
    ) -> Result<(), GraphicsError> {
        let mut types = Vec::with_capacity(heaps.len());
        for &heap in heaps {
            let heap_type = self.descriptor_heap(heap)?.heap_type;
            if types.contains(&heap_type) {
                return Err(GraphicsError::InvalidDescriptor);
            }
            types.push(heap_type);
        }
        self.recording_list(list)?
            .commands
            .push(GraphicsCommand::SetDescriptorHeaps {
                heaps: heaps.to_vec(),
            });
        Ok(())
    }

    /// Record `SetGraphicsRootSignature`.
    pub fn cmd_set_graphics_root_signature(
        &mut self,
        list: GraphicsHandle,
        root_signature: GraphicsHandle,
    ) -> Result<(), GraphicsError> {
        if !self.root_signatures.contains_key(&root_signature) {
            return Err(GraphicsError::InvalidDescriptor);
        }
        self.recording_list(list)?
            .commands
            .push(GraphicsCommand::SetGraphicsRootSignature { root_signature });
        Ok(())
    }

    /// Record `SetGraphicsRootDescriptorTable`. When the list executes, the
    /// command checks the base against the bound root signature and the bound
    /// heap of the table's type. A draw then reads the descriptor it names.
    pub fn cmd_set_graphics_root_descriptor_table(
        &mut self,
        list: GraphicsHandle,
        root_parameter_index: u32,
        base: GpuDescriptorHandle,
    ) -> Result<(), GraphicsError> {
        self.recording_list(list)?
            .commands
            .push(GraphicsCommand::SetGraphicsRootDescriptorTable {
                root_parameter_index,
                base,
            });
        Ok(())
    }

    /// Close command list.
    pub fn close_command_list(&mut self, list: GraphicsHandle) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if !cmd_list.is_recording {
            return Err(GraphicsError::CommandListAlreadyClosed);
        }
        cmd_list.is_recording = false;
        Ok(())
    }

    /// Reset command list with an allocator.
    pub fn reset_command_list(
        &mut self,
        list: GraphicsHandle,
        allocator: GraphicsHandle,
    ) -> Result<(), GraphicsError> {
        if !self.allocators.contains_key(&allocator) {
            return Err(GraphicsError::InvalidAllocatorHandle);
        }
        let cmd_list = self
            .command_lists
            .get_mut(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        cmd_list.commands.clear();
        cmd_list.is_recording = true;
        Ok(())
    }

    /// Destroy a guest device (`ID3D12Device` release). The device must have
    /// no live child object: command queues, command allocators, command
    /// lists, resources, descriptor heaps, root signatures or fences.
    pub fn destroy_device(&mut self, device: GraphicsHandle) -> Result<(), GraphicsError> {
        if !self.devices.contains_key(&device) {
            return Err(GraphicsError::InvalidDeviceHandle);
        }
        let has_children = self.queues.values().any(|queue| queue.device == device)
            || self.fences.values().any(|fence| fence.device == device)
            || self
                .allocators
                .values()
                .any(|allocator| allocator.device == device)
            || self
                .command_lists
                .values()
                .any(|list| list.device == device)
            || self.resources.values().any(|res| res.device == device)
            || self
                .descriptor_heaps
                .values()
                .any(|heap| heap.device == device)
            || self
                .root_signatures
                .values()
                .any(|signature| signature.device == device);
        if has_children {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.devices.remove(&device);
        Ok(())
    }

    /// Destroy a command queue (`ID3D12CommandQueue` release). Every command
    /// list the queue submitted must already be destroyed.
    pub fn destroy_command_queue(&mut self, queue: GraphicsHandle) -> Result<(), GraphicsError> {
        let cmd_queue = self
            .queues
            .get(&queue)
            .ok_or(GraphicsError::InvalidQueueHandle)?;
        if cmd_queue
            .submitted_lists
            .iter()
            .any(|list| self.command_lists.contains_key(list))
        {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.queues.remove(&queue);
        Ok(())
    }

    /// Destroy a command allocator (`ID3D12CommandAllocator` release). Every
    /// command list created from it must already be destroyed.
    pub fn destroy_command_allocator(
        &mut self,
        allocator: GraphicsHandle,
    ) -> Result<(), GraphicsError> {
        if !self.allocators.contains_key(&allocator) {
            return Err(GraphicsError::InvalidAllocatorHandle);
        }
        if self
            .command_lists
            .values()
            .any(|list| list.allocator == allocator)
        {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.allocators.remove(&allocator);
        Ok(())
    }

    /// Destroy a command list (`ID3D12GraphicsCommandList` release). The list
    /// must not still be recording.
    pub fn destroy_command_list(&mut self, list: GraphicsHandle) -> Result<(), GraphicsError> {
        let cmd_list = self
            .command_lists
            .get(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;
        if cmd_list.is_recording {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.command_lists.remove(&list);
        // The queue that submitted the list no longer holds it.
        for cmd_queue in self.queues.values_mut() {
            cmd_queue
                .submitted_lists
                .retain(|submitted| *submitted != list);
        }
        Ok(())
    }

    /// Destroy a committed resource (`ID3D12Resource` release). No command
    /// list that is still recording may reference it.
    pub fn destroy_resource(&mut self, resource: GraphicsHandle) -> Result<(), GraphicsError> {
        if !self.resources.contains_key(&resource) {
            return Err(GraphicsError::InvalidResourceHandle);
        }
        let referenced = self.command_lists.values().any(|list| {
            list.is_recording
                && list
                    .commands
                    .iter()
                    .any(|command| Self::command_references_resource(command, resource))
        });
        if referenced {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        self.resources.remove(&resource);
        Ok(())
    }

    /// The bytes a resource stores, for a guest that reads back its contents.
    pub fn resource_bytes(&self, resource: GraphicsHandle) -> Result<&[u8], GraphicsError> {
        let res = self
            .resources
            .get(&resource)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        Ok(&res.data)
    }

    /// Whether a recorded command names a resource.
    fn command_references_resource(command: &GraphicsCommand, resource: GraphicsHandle) -> bool {
        match command {
            GraphicsCommand::ResourceBarrier {
                resource: named, ..
            } => *named == resource,
            GraphicsCommand::SetRenderTargets { color_target, .. } => {
                color_target == &Some(resource)
            }
            GraphicsCommand::SetVertexBuffer {
                resource: named, ..
            } => *named == resource,
            GraphicsCommand::SetIndexBuffer {
                resource: named, ..
            } => *named == resource,
            _ => false,
        }
    }

    /// Execute command list on command queue using the configured backend.
    pub fn execute_command_list(
        &mut self,
        queue: GraphicsHandle,
        list: GraphicsHandle,
        memory: &GuestMemory,
    ) -> Result<ExecutionOutcome, GraphicsError> {
        let backend_closure = self.backend.clone();
        let backend_id = self.backend_id;
        let renderer_ref: Option<&BackendRenderer<'_>> = match &backend_closure {
            Some(arc) => {
                let r: &BackendRenderer<'_> = arc.as_ref();
                Some(r)
            }
            None => None,
        };
        self.execute_command_list_internal(queue, list, memory, renderer_ref, backend_id)
    }

    /// Execute command list strictly using native Metal. Fails with `GraphicsError::NativeMetalRequired`
    /// if a native Metal backend is not attached.
    pub fn execute_command_list_native_metal(
        &mut self,
        queue: GraphicsHandle,
        list: GraphicsHandle,
        memory: &GuestMemory,
        metal_renderer: &BackendRenderer<'_>,
    ) -> Result<ExecutionOutcome, GraphicsError> {
        self.execute_command_list_internal(
            queue,
            list,
            memory,
            Some(metal_renderer),
            GraphicsBackendId::MetalNative,
        )
    }

    /// Internal execution state machine.
    fn execute_command_list_internal(
        &mut self,
        queue: GraphicsHandle,
        list: GraphicsHandle,
        memory: &GuestMemory,
        backend_renderer: Option<&BackendRenderer<'_>>,
        backend_id: GraphicsBackendId,
    ) -> Result<ExecutionOutcome, GraphicsError> {
        let textured_backend = self.textured_backend.clone();
        let textured_renderer: Option<&TexturedBackendRenderer<'_>> = match &textured_backend {
            Some(arc) => Some(arc.as_ref()),
            None => None,
        };
        if !self.queues.contains_key(&queue) {
            return Err(GraphicsError::InvalidQueueHandle);
        }
        let cmd_list = self
            .command_lists
            .get(&list)
            .ok_or(GraphicsError::InvalidCommandListHandle)?;

        if cmd_list.is_recording {
            return Err(GraphicsError::CommandListNotRecording);
        }
        if cmd_list.commands.is_empty() {
            return Err(GraphicsError::EmptyCommandList);
        }

        self.apply_queue_waits(queue, backend_id)?;
        let cmd_list = self.command_lists.get(&list).unwrap();
        self.queues
            .get_mut(&queue)
            .unwrap()
            .submitted_lists
            .push(list);

        // State machine parsing recorded commands
        let mut viewport = (4usize, 3usize);
        let mut clear_color = [0u8, 0u8, 0u8, 255u8];
        let mut target_handle: Option<GraphicsHandle> = None;
        let mut vertex_resource: Option<GraphicsHandle> = None;
        let mut vertex_offset = 0usize;
        let mut vertex_stride = 0usize;
        let mut index_buffer: Option<(GraphicsHandle, usize, IndexFormat)> = None;
        let mut rendered_output: Option<Vec<u8>> = None;
        let mut draw_count = 0usize;
        let mut bindings = TableBindings::default();

        for cmd in &cmd_list.commands {
            match cmd {
                GraphicsCommand::ResourceBarrier {
                    resource,
                    before,
                    after,
                } => {
                    let res = self
                        .resources
                        .get_mut(resource)
                        .ok_or(GraphicsError::InvalidResourceHandle)?;
                    if res.state != *before && before != &ResourceStates::COMMON {
                        return Err(GraphicsError::ResourceLifetimeViolation);
                    }
                    res.state = *after;
                }
                GraphicsCommand::SetRenderTargets {
                    color_target,
                    clear_color: cc,
                } => {
                    target_handle = *color_target;
                    clear_color = *cc;
                }
                GraphicsCommand::SetViewport { width, height } => {
                    viewport = (*width, *height);
                }
                GraphicsCommand::SetVertexBuffer {
                    resource,
                    offset,
                    stride,
                } => {
                    vertex_resource = Some(*resource);
                    vertex_offset = *offset;
                    vertex_stride = *stride;
                }
                GraphicsCommand::SetIndexBuffer {
                    resource,
                    offset,
                    format,
                } => index_buffer = Some((*resource, *offset, *format)),
                GraphicsCommand::SetDescriptorHeaps { heaps } => {
                    let (mut srv_heap, mut sampler_heap) = (None, None);
                    for &heap in heaps {
                        match self.descriptor_heap(heap)?.heap_type {
                            DescriptorHeapType::CbvSrvUav => srv_heap = Some(heap.id),
                            DescriptorHeapType::Sampler => sampler_heap = Some(heap.id),
                        }
                    }
                    // Changing the heaps invalidates tables bound in the old
                    // ones; setting the same heaps again keeps them.
                    if (srv_heap, sampler_heap) != (bindings.srv_heap, bindings.sampler_heap) {
                        bindings.tables = [None; 2];
                    }
                    bindings.srv_heap = srv_heap;
                    bindings.sampler_heap = sampler_heap;
                }
                GraphicsCommand::SetGraphicsRootSignature { root_signature } => {
                    let layout = *self
                        .root_signatures
                        .get(root_signature)
                        .ok_or(GraphicsError::InvalidDescriptor)?;
                    // A different root signature invalidates every root
                    // argument; setting the bound one again keeps them.
                    if bindings.root_signature.map(|(handle, _)| handle) != Some(*root_signature) {
                        bindings.tables = [None; 2];
                    }
                    bindings.root_signature = Some((*root_signature, layout));
                }
                GraphicsCommand::SetGraphicsRootDescriptorTable {
                    root_parameter_index,
                    base,
                } => {
                    let (_, layout) = bindings
                        .root_signature
                        .ok_or(GraphicsError::InvalidDescriptor)?;
                    let bound_heap = if *root_parameter_index == layout.srv_parameter {
                        bindings.srv_heap
                    } else if *root_parameter_index == layout.sampler_parameter {
                        bindings.sampler_heap
                    } else {
                        return Err(GraphicsError::InvalidDescriptor);
                    };
                    // The base must name a slot of the bound heap of the
                    // table's type; its contents are read at the draw.
                    let (heap_id, index) = Self::decode_descriptor_handle(base.0, true)
                        .ok_or(GraphicsError::InvalidDescriptor)?;
                    if bound_heap != Some(heap_id)
                        || index >= self.descriptor_heaps[&heap_id].slots.len()
                    {
                        return Err(GraphicsError::InvalidDescriptor);
                    }
                    bindings.tables[*root_parameter_index as usize] = Some(*base);
                }
                GraphicsCommand::DrawIndexedInstanced {
                    index_count_per_instance,
                    instance_count,
                    start_index_location,
                    base_vertex_location,
                    start_instance_location,
                } => {
                    if !matches!(index_count_per_instance, 0 | 3)
                        || *instance_count > 1
                        || *start_instance_location != 0
                    {
                        return Err(GraphicsError::InvalidDescriptor);
                    }
                    if *index_count_per_instance == 0 || *instance_count == 0 {
                        continue;
                    }
                    let (index_handle, index_offset, format) =
                        index_buffer.ok_or(GraphicsError::InvalidResourceHandle)?;
                    let index_resource = self
                        .resources
                        .get(&index_handle)
                        .ok_or(GraphicsError::InvalidResourceHandle)?;
                    let index_view = (index_resource, index_offset, format);
                    let index_range = (*start_index_location, *base_vertex_location);
                    let vertex_view = (vertex_resource, vertex_offset, vertex_stride);
                    let pixels = if let Some((_, root_signature)) = bindings.root_signature {
                        let indices =
                            Self::resolve_triangle_indices(memory, index_view, index_range)?;
                        self.textured_draw(
                            memory,
                            &bindings,
                            root_signature,
                            vertex_view,
                            indices,
                            (viewport, clear_color),
                            (textured_renderer, backend_id),
                        )?
                    } else {
                        let vertices = self.fetch_indexed_positions(
                            memory,
                            vertex_view,
                            index_view,
                            index_range,
                        )?;
                        if let Some(renderer) = backend_renderer {
                            renderer(&vertices, viewport, clear_color)?
                        } else if backend_id == GraphicsBackendId::MetalNative {
                            return Err(GraphicsError::NativeMetalRequired);
                        } else {
                            Self::execute_software_reference_draw(&vertices, viewport, clear_color)?
                        }
                    };
                    draw_count += 1;
                    if let Some(th) = target_handle
                        && let Some(target_res) = self.resources.get_mut(&th)
                        && target_res.data.len() >= pixels.len()
                    {
                        target_res.data[..pixels.len()].copy_from_slice(&pixels);
                    }
                    rendered_output = Some(pixels);
                }
                GraphicsCommand::DrawInstanced {
                    vertex_count_per_instance,
                    instance_count,
                    start_vertex_location,
                    start_instance_location,
                } => {
                    if let Some((_, root_signature)) = bindings.root_signature {
                        if *vertex_count_per_instance == 0 || *instance_count == 0 {
                            continue;
                        }
                        if *vertex_count_per_instance != 3
                            || *instance_count != 1
                            || *start_instance_location != 0
                        {
                            return Err(GraphicsError::Unsupported(
                                "textured draw other than one instance of three vertices",
                            ));
                        }
                        let first = *start_vertex_location as usize;
                        let pixels = self.textured_draw(
                            memory,
                            &bindings,
                            root_signature,
                            (vertex_resource, vertex_offset, vertex_stride),
                            [first, first + 1, first + 2],
                            (viewport, clear_color),
                            (textured_renderer, backend_id),
                        )?;
                        draw_count += 1;
                        if let Some(th) = target_handle
                            && let Some(target_res) = self.resources.get_mut(&th)
                            && target_res.data.len() >= pixels.len()
                        {
                            target_res.data[..pixels.len()].copy_from_slice(&pixels);
                        }
                        rendered_output = Some(pixels);
                        continue;
                    }
                    // Zero instances, or fewer vertices than one triangle,
                    // draw nothing. The flat path draws only the first
                    // triangle of the view, once; anything else is rejected
                    // rather than drawn as a different triangle.
                    if *vertex_count_per_instance < 3 || *instance_count == 0 {
                        continue;
                    }
                    if *vertex_count_per_instance != 3
                        || *instance_count != 1
                        || *start_vertex_location != 0
                        || *start_instance_location != 0
                    {
                        return Err(GraphicsError::Unsupported(
                            "flat draw other than one instance of the view's first three vertices",
                        ));
                    }
                    draw_count += 1;
                    let v_res_handle =
                        vertex_resource.ok_or(GraphicsError::InvalidResourceHandle)?;
                    let v_res = self
                        .resources
                        .get(&v_res_handle)
                        .ok_or(GraphicsError::InvalidResourceHandle)?;

                    // Three float4 positions, inside the resource whether it
                    // is backed by guest memory or by its own bytes.
                    let mut raw_vtx = [0u8; 48];
                    Self::read_resource(v_res, memory, vertex_offset, &mut raw_vtx)?;

                    let mut vertices = [[0.0f32; 4]; 3];
                    for (i, row) in vertices.iter_mut().enumerate() {
                        for (j, val) in row.iter_mut().enumerate() {
                            let off = (i * 4 + j) * 4;
                            *val = f32::from_le_bytes(raw_vtx[off..off + 4].try_into().unwrap());
                        }
                    }

                    let pixels = if let Some(renderer) = backend_renderer {
                        renderer(&vertices, viewport, clear_color)?
                    } else if backend_id == GraphicsBackendId::MetalNative {
                        return Err(GraphicsError::NativeMetalRequired);
                    } else {
                        Self::execute_software_reference_draw(&vertices, viewport, clear_color)?
                    };

                    // Update target resource if present
                    if let Some(th) = target_handle
                        && let Some(target_res) = self.resources.get_mut(&th)
                        && target_res.data.len() >= pixels.len()
                    {
                        target_res.data[..pixels.len()].copy_from_slice(&pixels);
                    }

                    rendered_output = Some(pixels);
                }
            }
        }

        let out = rendered_output.unwrap_or_else(|| {
            let total = viewport.0 * viewport.1;
            clear_color.repeat(total)
        });

        // Store into last presentation
        if let Ok(mut pres) = self.last_presentation.lock() {
            pres.width = viewport.0;
            pres.height = viewport.1;
            pres.pixels = out.clone();
            pres.frame_index += 1;
            pres.draw_count = draw_count;
            pres.backend = Some(backend_id);
        }

        Ok(ExecutionOutcome {
            backend: backend_id,
            pixels: out,
            width: viewport.0,
            height: viewport.1,
            draw_count,
        })
    }

    /// Decode three indices from the bound view and apply the signed base vertex.
    fn resolve_triangle_indices(
        memory: &GuestMemory,
        (index_res, index_offset, format): (&GraphicsResource, usize, IndexFormat),
        (start, base_vertex): (u32, i32),
    ) -> Result<[usize; 3], GraphicsError> {
        let size = format.size();
        let mut raw_indices = [0u8; 12];
        let raw_indices = &mut raw_indices[..3 * size];
        let first = (start as usize)
            .checked_mul(size)
            .and_then(|bytes| bytes.checked_add(index_offset))
            .ok_or(GraphicsError::IndexOutOfRange)?;
        Self::read_resource(index_res, memory, first, raw_indices)?;
        let mut indices = [0; 3];
        for (vertex, encoded) in indices.iter_mut().zip(raw_indices.chunks_exact(size)) {
            let index = match format {
                IndexFormat::R16Uint => u32::from(u16::from_le_bytes(encoded.try_into().unwrap())),
                IndexFormat::R32Uint => u32::from_le_bytes(encoded.try_into().unwrap()),
            };
            *vertex = usize::try_from(i64::from(index) + i64::from(base_vertex))
                .map_err(|_| GraphicsError::IndexOutOfRange)?;
        }
        Ok(indices)
    }

    /// Fetch the float4 position of each vertex selected by the index view.
    fn fetch_indexed_positions(
        &self,
        memory: &GuestMemory,
        (vertex_resource, vertex_offset, vertex_stride): (Option<GraphicsHandle>, usize, usize),
        index_view: (&GraphicsResource, usize, IndexFormat),
        index_range: (u32, i32),
    ) -> Result<[[f32; 4]; 3], GraphicsError> {
        let vertex_res = vertex_resource
            .and_then(|handle| self.resources.get(&handle))
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        if vertex_stride < 16 {
            return Err(GraphicsError::InvalidDescriptor);
        }
        // Preserve flat draws' binding validation before reading index data.
        let indices = Self::resolve_triangle_indices(memory, index_view, index_range)?;
        let mut vertices = [[0.0f32; 4]; 3];
        for (row, vertex) in vertices.iter_mut().zip(indices) {
            let at = vertex
                .checked_mul(vertex_stride)
                .and_then(|bytes| bytes.checked_add(vertex_offset))
                .ok_or(GraphicsError::IndexOutOfRange)?;
            let mut raw = [0u8; 16];
            Self::read_resource(vertex_res, memory, at, &mut raw)?;
            for (value, bytes) in row.iter_mut().zip(raw.as_chunks::<4>().0) {
                *value = f32::from_le_bytes(*bytes);
            }
        }
        Ok(vertices)
    }

    /// Resolve the SRV and sampler tables, fetch one textured triangle and
    /// draw it on the textured backend, or on the software reference when no
    /// native backend is required. Every check runs before any drawing.
    #[allow(clippy::too_many_arguments)]
    fn textured_draw(
        &self,
        memory: &GuestMemory,
        bindings: &TableBindings,
        root_signature: RootSignature,
        (vertex_resource, vertex_offset, vertex_stride): (Option<GraphicsHandle>, usize, usize),
        vertex_indices: [usize; 3],
        (viewport, clear_color): ((usize, usize), [u8; 4]),
        (renderer, backend_id): (Option<&TexturedBackendRenderer<'_>>, GraphicsBackendId),
    ) -> Result<Vec<u8>, GraphicsError> {
        let srv = self.resolve_table(bindings, root_signature.srv_parameter, bindings.srv_heap)?;
        let sampler = self.resolve_table(
            bindings,
            root_signature.sampler_parameter,
            bindings.sampler_heap,
        )?;
        let (Descriptor::Srv { resource, format }, Descriptor::Sampler(sampler)) = (srv, sampler)
        else {
            return Err(GraphicsError::InvalidDescriptor);
        };
        let texture = self
            .resources
            .get(&resource)
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        // COMMON promotes to a shader-resource read. Any other state must
        // include PIXEL_SHADER_RESOURCE and combine it only with read states.
        let state = texture.state.0;
        if state != ResourceStates::COMMON.0
            && (state & ResourceStates::PIXEL_SHADER_RESOURCE.0 == 0 || state & !READ_STATES != 0)
        {
            return Err(GraphicsError::ResourceLifetimeViolation);
        }
        let width = texture.desc.width as u32;
        let height = texture.desc.height;
        let mut bytes = vec![0; format.level_size(width, height)];
        Self::read_resource(texture, memory, 0, &mut bytes)?;

        let vertex_res = vertex_resource
            .and_then(|handle| self.resources.get(&handle))
            .ok_or(GraphicsError::InvalidResourceHandle)?;
        if vertex_stride < 32 {
            return Err(GraphicsError::InvalidDescriptor);
        }
        let mut positions = [[0.0f32; 4]; 3];
        let mut texcoords = [[0.0f32; 4]; 3];
        for ((position, texcoord), vertex) in
            positions.iter_mut().zip(&mut texcoords).zip(vertex_indices)
        {
            let at = vertex
                .checked_mul(vertex_stride)
                .and_then(|bytes| bytes.checked_add(vertex_offset))
                .ok_or(GraphicsError::IndexOutOfRange)?;
            let mut raw = [0u8; 32];
            Self::read_resource(vertex_res, memory, at, &mut raw)?;
            for (value, bytes) in position
                .iter_mut()
                .chain(texcoord.iter_mut())
                .zip(raw.as_chunks::<4>().0)
            {
                *value = f32::from_le_bytes(*bytes);
            }
        }

        let draw = TexturedDraw {
            positions,
            texcoords,
            viewport,
            clear_color,
            texture: TextureView {
                format,
                width,
                height,
                bytes: &bytes,
            },
            sampler,
        };
        if let Some(renderer) = renderer {
            renderer(&draw)
        } else if backend_id == GraphicsBackendId::MetalNative {
            Err(GraphicsError::NativeMetalRequired)
        } else {
            Self::execute_software_reference_textured_draw(&draw)
        }
    }

    /// The descriptor that the table at `parameter` names in the bound heap.
    fn resolve_table(
        &self,
        bindings: &TableBindings,
        parameter: u32,
        bound_heap: Option<u32>,
    ) -> Result<Descriptor, GraphicsError> {
        let base = bindings.tables[parameter as usize].ok_or(GraphicsError::InvalidDescriptor)?;
        let (heap_id, index) =
            Self::decode_descriptor_handle(base.0, true).ok_or(GraphicsError::InvalidDescriptor)?;
        if bound_heap != Some(heap_id) {
            return Err(GraphicsError::InvalidDescriptor);
        }
        self.descriptor_heaps
            .get(&heap_id)
            .and_then(|heap| heap.slots.get(index).copied().flatten())
            .ok_or(GraphicsError::InvalidDescriptor)
    }

    /// Software reference for a textured triangle: the coverage rule of
    /// [`Self::execute_software_reference_draw`], affine interpolation of the
    /// texture coordinate at each pixel center, and point sampling with
    /// clamp addressing (`texel = min(floor(clamp(u, 0, 1) * width), width - 1)`).
    /// BC1 levels are decoded with `xenora_texture::decode_bc1`. Positions
    /// must have `w == 1`.
    pub fn execute_software_reference_textured_draw(
        draw: &TexturedDraw<'_>,
    ) -> Result<Vec<u8>, GraphicsError> {
        if draw.positions.iter().any(|position| position[3] != 1.0) {
            return Err(GraphicsError::Unsupported(
                "software textured draw with w other than 1",
            ));
        }
        if draw
            .positions
            .iter()
            .chain(&draw.texcoords)
            .flatten()
            .any(|value| !value.is_finite())
        {
            return Err(GraphicsError::InvalidDescriptor);
        }
        let max = MAX_TEXTURE_DIMENSION;
        if !(1..=max).contains(&draw.texture.width) || !(1..=max).contains(&draw.texture.height) {
            return Err(GraphicsError::Unsupported("texture size"));
        }
        let texels = decode_texture(&draw.texture)?;
        let (tex_w, tex_h) = (draw.texture.width as usize, draw.texture.height as usize);
        let (w, h) = draw.viewport;
        let count = w
            .checked_mul(h)
            .filter(|&count| (1..=MAX_SOFTWARE_TEXTURED_PIXELS).contains(&count))
            .ok_or(GraphicsError::InvalidViewport)?;
        let mut pixels = draw.clear_color.repeat(count);
        let window = |p: [f32; 4]| [(p[0] + 1.0) * 0.5 * w as f32, (1.0 - p[1]) * 0.5 * h as f32];
        let [p0, p1, p2] = draw.positions.map(window);
        let edge = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| -> f32 {
            (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
        };
        let area = edge(p0, p1, p2);
        if area.abs() < 1e-5 {
            return Ok(pixels);
        }
        let nearest = |coord: f32, size: usize| {
            ((f64::from(coord.clamp(0.0, 1.0)) * size as f64).floor() as usize).min(size - 1)
        };
        for y in 0..h {
            for x in 0..w {
                let pt = [x as f32 + 0.5, y as f32 + 0.5];
                let b = [edge(p1, p2, pt), edge(p2, p0, pt), edge(p0, p1, pt)];
                let inside = if area > 0.0 {
                    b.iter().all(|&e| e >= 0.0)
                } else {
                    b.iter().all(|&e| e <= 0.0)
                };
                if !inside {
                    continue;
                }
                let [u, v] = [0, 1].map(|axis| {
                    (0..3)
                        .map(|k| b[k] / area * draw.texcoords[k][axis])
                        .sum::<f32>()
                });
                let texel = (nearest(v, tex_h) * tex_w + nearest(u, tex_w)) * 4;
                let idx = (y * w + x) * 4;
                pixels[idx..idx + 4].copy_from_slice(&texels[texel..texel + 4]);
            }
        }
        Ok(pixels)
    }

    /// Read `out.len()` bytes at `offset` of a resource, from its guest backing
    /// when it has one. The range must lie inside the resource's size.
    fn read_resource(
        res: &GraphicsResource,
        memory: &GuestMemory,
        offset: usize,
        out: &mut [u8],
    ) -> Result<(), GraphicsError> {
        let end = offset
            .checked_add(out.len())
            .filter(|&end| end <= res.data.len())
            .ok_or(GraphicsError::IndexOutOfRange)?;
        match res.guest_backing_addr {
            Some(guest_addr) => {
                let address = guest_addr
                    .checked_add(offset as u64)
                    .filter(|address| address.checked_add(out.len() as u64).is_some())
                    .ok_or(GraphicsError::IndexOutOfRange)?;
                memory.read_bytes(address, out)?;
            }
            None => out.copy_from_slice(&res.data[offset..end]),
        }
        Ok(())
    }

    /// Software reference rasterizer for deterministic test verification when GPU tests are off.
    pub fn execute_software_reference_draw(
        vertices: &[[f32; 4]; 3],
        viewport: (usize, usize),
        clear_color: [u8; 4],
    ) -> Result<Vec<u8>, GraphicsError> {
        let (w, h) = viewport;
        let mut pixels = clear_color.repeat(w * h);

        // Convert clip-space coordinates to window pixels [0, w-1] x [0, h-1]
        let p0 = [
            (vertices[0][0] + 1.0) * 0.5 * w as f32,
            (1.0 - vertices[0][1]) * 0.5 * h as f32,
        ];
        let p1 = [
            (vertices[1][0] + 1.0) * 0.5 * w as f32,
            (1.0 - vertices[1][1]) * 0.5 * h as f32,
        ];
        let p2 = [
            (vertices[2][0] + 1.0) * 0.5 * w as f32,
            (1.0 - vertices[2][1]) * 0.5 * h as f32,
        ];

        // 2D barycentric edge functions
        let edge = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| -> f32 {
            (c[0] - a[0]) * (b[1] - a[1]) - (c[1] - a[1]) * (b[0] - a[0])
        };

        let area = edge(p0, p1, p2);
        if area.abs() < 1e-5 {
            return Ok(pixels);
        }

        for y in 0..h {
            for x in 0..w {
                let pt = [x as f32 + 0.5, y as f32 + 0.5];
                let w0 = edge(p1, p2, pt);
                let w1 = edge(p2, p0, pt);
                let w2 = edge(p0, p1, pt);

                let inside = if area > 0.0 {
                    w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0
                } else {
                    w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0
                };

                if inside {
                    let idx = (y * w + x) * 4;
                    pixels[idx..idx + 4].copy_from_slice(&[255, 0, 0, 255]);
                }
            }
        }

        Ok(pixels)
    }
}

/// Row-major RGBA8 texels of a bound texture level.
fn decode_texture(texture: &TextureView<'_>) -> Result<Vec<u8>, GraphicsError> {
    let (width, height) = (texture.width as usize, texture.height as usize);
    if texture.bytes.len() != texture.format.level_size(texture.width, texture.height) {
        return Err(GraphicsError::InvalidDescriptor);
    }
    match texture.format {
        TextureFormat::Rgba8Unorm => Ok(texture.bytes.to_vec()),
        TextureFormat::Bc1Unorm => {
            let blocks_wide = width.div_ceil(4);
            let mut texels = vec![0; width * height * 4];
            for (block_index, block) in texture.bytes.as_chunks::<8>().0.iter().enumerate() {
                let decoded = xenora_texture::decode_bc1(*block);
                let (bx, by) = (block_index % blocks_wide * 4, block_index / blocks_wide * 4);
                for (i, texel) in decoded.iter().enumerate() {
                    let (x, y) = (bx + i % 4, by + i / 4);
                    if x < width && y < height {
                        let at = (y * width + x) * 4;
                        texels[at..at + 4].copy_from_slice(texel);
                    }
                }
            }
            Ok(texels)
        }
    }
}
