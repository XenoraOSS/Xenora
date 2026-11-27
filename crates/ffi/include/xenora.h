/* SPDX-License-Identifier: GPL-3.0-or-later */
/*
 * Xenora native application interface, version 1.
 *
 * Every function returns a xenora_status. Sessions are identified by opaque
 * 64-bit handles, never by pointers; 0 is never a valid handle. A destroyed
 * handle stays invalid even if its slot is reused. Output pointers are owned
 * by the caller and are written only when the call returns XENORA_OK.
 * All functions are thread-safe. No Rust panic crosses this boundary: one is
 * reported as XENORA_INTERNAL_ERROR.
 */
#ifndef XENORA_H
#define XENORA_H

#include <stdint.h>

#define XENORA_ABI_VERSION 1u

typedef int32_t xenora_status;
enum {
    XENORA_OK = 0,
    XENORA_INVALID_ARGUMENT = 1,
    XENORA_INVALID_HANDLE = 2,
    XENORA_VERSION_MISMATCH = 3,
    XENORA_INTERNAL_ERROR = 4,
    XENORA_PE_ERROR = 5,
    XENORA_LIMIT_EXCEEDED = 6,
    XENORA_BUFFER_TOO_SMALL = 7,
    XENORA_EXECUTION_FAILED = 8,
    XENORA_INVALID_STATE = 9,
    XENORA_FILE_NOT_FOUND = 10,
    XENORA_ACCESS_DENIED = 11,
    XENORA_IO_ERROR = 12,
};

/* Frame-loop states reported by xenora_session_frame_loop_status. */
enum {
    XENORA_LOOP_NONE = 0,
    XENORA_LOOP_RUNNING = 1,
    XENORA_LOOP_PAUSED = 2,
    XENORA_LOOP_FINISHED = 3,
    XENORA_LOOP_STOPPED = 4,
    XENORA_LOOP_FAILED = 5,
};

#define XENORA_FRAME_LOOP_MAX_FRAMES 100000u
/* Frame id reported when no frame has been completed yet. */
#define XENORA_NO_FRAME 0xFFFFFFFFu

/* Input snapshot delivered to guest runtime services (Gate 4, #276). */
typedef struct {
    uint32_t buttons;
    int16_t stick_x;
    int16_t stick_y;
    int16_t mouse_dx;
    int16_t mouse_dy;
    uint8_t mouse_buttons;
    uint8_t connected;
    uint16_t reserved;
} xenora_input_state;

/* Extended controller snapshot. Trigger values span 0 to 65535. */
typedef struct {
    xenora_input_state state;
    uint16_t left_trigger;
    uint16_t right_trigger;
} xenora_controller_input;

/* Controller snapshot in public XInput units for guest runtime services.
 * Trigger values span 0 to 255. `connected` must be 0 or 1, `features` may
 * contain only XENORA_XINPUT_FEATURE_* bits and reserved bytes must be zero.
 * Unlike xenora_controller_input, this carries both thumbsticks
 * and uses the public XInput button bit assignments directly.
 */
typedef struct {
    uint16_t buttons;
    uint8_t left_trigger;
    uint8_t right_trigger;
    int16_t left_x;
    int16_t left_y;
    int16_t right_x;
    int16_t right_y;
    uint8_t connected;
    uint8_t features;
    uint8_t reserved[2];
} xenora_xinput_state;

#define XENORA_XINPUT_FEATURE_VIBRATION 0x01u

#define XENORA_XINPUT_BATTERY_STATE_UNKNOWN 0u
#define XENORA_XINPUT_BATTERY_STATE_DISCHARGING 1u
#define XENORA_XINPUT_BATTERY_STATE_CHARGING 2u
#define XENORA_XINPUT_BATTERY_STATE_FULL 3u
#define XENORA_XINPUT_BATTERY_LEVEL_UNKNOWN 255u

/* Host battery evidence for one XInput controller. present=0 means the host
 * exposed no battery object and requires zero state/level. For a present
 * object, level is 0..100 or XENORA_XINPUT_BATTERY_LEVEL_UNKNOWN. Charging
 * state is host evidence only and is never battery chemistry.
 */
typedef struct {
    uint8_t present;
    uint8_t state;
    uint8_t level;
    uint8_t reserved;
} xenora_xinput_battery;

/* Latest vibration command accepted from the guest. When `connected` is zero,
 * sequence and motor values are zero and the host must stop actuators. When
 * `enabled` is zero the motor values are registered but must not actuate;
 * output stops until XInput reporting is re-enabled. The sequence increments
 * for every accepted command while the virtual slot stays connected. Reserved
 * bytes are zero in output written by this library.
 */
typedef struct {
    uint32_t sequence;
    uint16_t left_motor;
    uint16_t right_motor;
    uint8_t connected;
    uint8_t enabled;
    uint8_t reserved[2];
} xenora_xinput_vibration;

#define XENORA_INPUT_NONE     0u
#define XENORA_INPUT_RIGHT    (1u << 0)
#define XENORA_INPUT_LEFT     (1u << 1)
#define XENORA_INPUT_UP       (1u << 2)
#define XENORA_INPUT_DOWN     (1u << 3)
#define XENORA_INPUT_ACTION_A (1u << 4)
#define XENORA_INPUT_ACTION_B (1u << 5)

/* Public XInput button bit assignments carried by xenora_xinput_state. */
#define XENORA_XINPUT_GAMEPAD_DPAD_UP 0x0001u
#define XENORA_XINPUT_GAMEPAD_DPAD_DOWN 0x0002u
#define XENORA_XINPUT_GAMEPAD_DPAD_LEFT 0x0004u
#define XENORA_XINPUT_GAMEPAD_DPAD_RIGHT 0x0008u
#define XENORA_XINPUT_GAMEPAD_START 0x0010u
#define XENORA_XINPUT_GAMEPAD_BACK 0x0020u
#define XENORA_XINPUT_GAMEPAD_LEFT_THUMB 0x0040u
#define XENORA_XINPUT_GAMEPAD_RIGHT_THUMB 0x0080u
#define XENORA_XINPUT_GAMEPAD_LEFT_SHOULDER 0x0100u
#define XENORA_XINPUT_GAMEPAD_RIGHT_SHOULDER 0x0200u
#define XENORA_XINPUT_GAMEPAD_GUIDE 0x0400u
#define XENORA_XINPUT_GAMEPAD_A 0x1000u
#define XENORA_XINPUT_GAMEPAD_B 0x2000u
#define XENORA_XINPUT_GAMEPAD_X 0x4000u
#define XENORA_XINPUT_GAMEPAD_Y 0x8000u

/* Size of the frame drawn by the trusted fixture: RGBA8, rows top to bottom. */
#define XENORA_FIXTURE_FRAME_WIDTH 4u
#define XENORA_FIXTURE_FRAME_HEIGHT 3u

typedef uint64_t xenora_session;

/* The interface version this library implements. */
uint32_t xenora_abi_version(void);

/* Create a session. `abi_version` must equal XENORA_ABI_VERSION. */
xenora_status xenora_session_create(uint32_t abi_version, xenora_session *out_session);

/* Destroy a session. Destroying it again returns XENORA_INVALID_HANDLE. */
xenora_status xenora_session_destroy(xenora_session session);

/* Parse the bundled trusted PE fixture with the runtime loader. No guest code runs. */
xenora_status xenora_session_inspect_trusted_fixture(
    xenora_session session,
    uint32_t *out_section_count,
    uint32_t *out_entry_rva);

/*
 * Run the bundled trusted PE fixture: its x86-64 code is translated and executed,
 * and its single draw request is rendered on the native Metal device. On XENORA_OK,
 * `out_rgba` holds width * height * 4 bytes of the read-back frame, which has been
 * checked against the fixture shader's known output. `capacity` is the size of
 * `out_rgba` in bytes. Blocks the calling thread; call it off the main thread.
 * Only the embedded fixture can run; no caller-supplied executable is accepted.
 */
xenora_status xenora_session_run_trusted_fixture(
    xenora_session session,
    uint8_t *out_rgba,
    uint64_t capacity,
    uint32_t *out_width,
    uint32_t *out_height);

/*
 * Persistent rendering (one frame loop per session). Start runs the embedded
 * frame-loop fixture on a session-owned guest thread: the guest submits frames
 * 0, 1, 2, ... and each is rendered on native Metal and verified before it
 * becomes the latest frame. The guest stops after `frame_count` frames
 * (1..=XENORA_FRAME_LOOP_MAX_FRAMES). Pause holds the guest at its next frame
 * submission; resume continues it; stop makes it leave its loop and halt, and
 * waits for its thread. Destroying the session stops the loop first. Starting a
 * second loop on the same session returns XENORA_INVALID_STATE.
 */
xenora_status xenora_session_start_frame_loop(xenora_session session, uint32_t frame_count);
xenora_status xenora_session_start_interactive_loop(xenora_session session, uint32_t frame_count);
xenora_status xenora_session_send_input(xenora_session session, const xenora_input_state *input);
xenora_status xenora_session_send_controller_input(xenora_session session, const xenora_controller_input *input);
/* Install or remove a full XInput-unit controller snapshot for user_index 0-3
 * in the running frame loop's guest runtime services. The legacy fixture input
 * above is not changed. Returns XENORA_INVALID_ARGUMENT for an invalid index,
 * connected value, feature bits or reserved bytes, and XENORA_INVALID_STATE when no loop runs.
 */
xenora_status xenora_session_send_xinput_state(xenora_session session, uint32_t user_index, const xenora_xinput_state *input);
/* Atomically replace one XInput snapshot and its latest battery evidence. */
xenora_status xenora_session_send_xinput_state_with_battery(
    xenora_session session,
    uint32_t user_index,
    const xenora_xinput_state *input,
    const xenora_xinput_battery *battery);
/* Poll the latest vibration command for user_index 0-3 without consuming it.
 * A disconnected slot succeeds with a zeroed disconnected command so the host
 * can stop output. Returns XENORA_INVALID_STATE when no loop runs.
 */
xenora_status xenora_session_poll_xinput_vibration(xenora_session session, uint32_t user_index, xenora_xinput_vibration *output);
xenora_status xenora_session_queue_input(xenora_session session, const xenora_input_state *input);
xenora_status xenora_session_pause_frame_loop(xenora_session session);
xenora_status xenora_session_resume_frame_loop(xenora_session session);
xenora_status xenora_session_stop_frame_loop(xenora_session session);

/*
 * Retrieve a previously completed frame by sequence index (0 <= frame_index < completed).
 * Used for deterministic replay verification and frame inspection.
 */
xenora_status xenora_session_get_recorded_frame(
    xenora_session session,
    uint32_t frame_index,
    uint32_t *out_frame_id,
    uint8_t *out_rgba,
    uint64_t capacity
);

/*
 * Report the loop's state, the number of completed frames, and the latest
 * verified frame: its id (XENORA_NO_FRAME if none yet) and, when one exists,
 * its RGBA8 pixels (XENORA_FIXTURE_FRAME_WIDTH x HEIGHT) in `out_rgba`.
 */
xenora_status xenora_session_frame_loop_status(
    xenora_session session,
    uint32_t *out_state,
    uint32_t *out_completed,
    uint32_t *out_frame_id,
    uint8_t *out_rgba,
    uint64_t capacity);

/*
 * Read the verified guest journal once a frame loop has cleanly finished or
 * stopped. Each byte records one completed frame. The low two bits hold
 * the column. Standard mode uses 0x10; interactive neutral, right and left
 * use 0x20, 0x24 and 0x28 respectively, before adding the column.
 *
 * out_required is mandatory. Query with NULL out_records and zero capacity.
 * An insufficient buffer returns XENORA_BUFFER_TOO_SMALL, reports the full
 * size and leaves out_records unchanged. A verified empty journal returns
 * XENORA_OK with size zero. No finalized journal returns XENORA_INVALID_STATE
 * and size zero. The maximum size is XENORA_FRAME_LOOP_MAX_FRAMES bytes.
 */
xenora_status xenora_session_frame_loop_journal(
    xenora_session session,
    uint8_t *out_records,
    uint64_t capacity,
    uint64_t *out_required);

/* Largest file xenora_inspect_executable accepts, in bytes. */
#define XENORA_MAX_INSPECT_BYTES 268435456u

/* Opaque persistent GPU resource handle (Gate 5, #279) */
typedef uint64_t xenora_resource;
#define XENORA_NO_RESOURCE 0u

enum {
    XENORA_RESOURCE_TYPE_TEXTURE_2D = 1,
    XENORA_RESOURCE_TYPE_BUFFER = 2,
};

/*
 * Create a persistent RGBA8 2D texture owned by the session.
 * If `initial_rgba` is non-null, it must point to at least width * height * 4 bytes.
 */
xenora_status xenora_session_create_texture(
    xenora_session session,
    uint32_t width,
    uint32_t height,
    const uint8_t *initial_rgba,
    xenora_resource *out_resource
);

/*
 * Dynamically update a subregion of a persistent texture.
 */
xenora_status xenora_session_update_texture(
    xenora_session session,
    xenora_resource resource,
    uint32_t x,
    uint32_t y,
    uint32_t width,
    uint32_t height,
    const uint8_t *rgba,
    uint64_t byte_count
);

/*
 * Destroy or retire a persistent resource. If in-flight GPU execution is using
 * the resource, destruction is deferred until the execution completes.
 */
xenora_status xenora_session_destroy_resource(
    xenora_session session,
    xenora_resource resource
);

/*
 * Query type, dimensions, and in-flight status of a persistent resource.
 */
xenora_status xenora_session_resource_info(
    xenora_session session,
    xenora_resource resource,
    uint32_t *out_type,
    uint32_t *out_width,
    uint32_t *out_height,
    uint32_t *out_in_flight
);

/*
 * Render a frame using a persistent texture resource.
 */
xenora_status xenora_session_render_textured(
    xenora_session session,
    xenora_resource resource,
    uint8_t *out_rgba,
    uint64_t capacity,
    uint32_t *out_width,
    uint32_t *out_height
);

/*
 * Run the original textured PE fixture and verify its read-back frame.
 */
xenora_status xenora_session_run_textured_fixture(
    xenora_session session,
    uint8_t *out_rgba,
    uint64_t capacity,
    uint32_t *out_width,
    uint32_t *out_height
);

/* Audio subsystem (Gate 6, #284) */
typedef uint32_t xenora_audio_format;
enum {
    XENORA_AUDIO_FORMAT_PCM_S16LE = 1,
    XENORA_AUDIO_FORMAT_PCM_F32LE = 2,
};

/*
 * Initialize the audio subsystem for a session.
 *
 * Reinitialization discards queued PCM and resets the underrun count. The
 * volume, mute state and cumulative submitted-byte count are preserved. The
 * new sample rate, channel count and format take effect immediately.
 */
xenora_status xenora_session_audio_init(
    xenora_session session,
    uint32_t sample_rate,
    uint32_t channels,
    xenora_audio_format format
);

/*
 * Submit PCM audio bytes into the session audio queue.
 * byte_count must contain a whole number of interleaved sample frames.
 *
 * The queue stores raw PCM and keeps at most the newest 256 KiB. An oversized
 * submission discards its oldest bytes and enough older queued bytes to fit.
 * The submitted-byte count includes discarded bytes. Gain and mute are applied
 * when frames are read or rendered, not when they are submitted.
 */
xenora_status xenora_session_audio_submit(
    xenora_session session,
    const uint8_t *pcm_bytes,
    uint64_t byte_count
);

/*
 * Read PCM audio bytes from the session audio queue.
 * Only whole frames are returned. Any remaining output bytes are untouched.
 *
 * The current volume and mute state are applied to the returned frames. Muted
 * frames still count as read. Frames queued before a control change use the
 * setting in effect when they are read.
 */
xenora_status xenora_session_audio_read(
    xenora_session session,
    uint8_t *out_pcm,
    uint64_t capacity,
    uint64_t *out_bytes_read
);

/*
 * Fill one playback block with whole frames, padding a shortfall with silence.
 * out_source_bytes reports how much PCM was supplied before padding.
 *
 * Current volume and mute are applied to supplied frames. Muted frames count
 * as supplied; a shortfall still increments the underrun count.
 */
xenora_status xenora_session_audio_render(
    xenora_session session,
    uint8_t *out_pcm,
    uint64_t byte_count,
    uint64_t *out_source_bytes
);

/*
 * Query queued bytes, render calls that ran short, and total bytes submitted.
 * The queued count is raw PCM. The total includes bytes discarded by the
 * retention bound or by reinitialization.
 */
xenora_status xenora_session_audio_status(
    xenora_session session,
    uint32_t *out_queued_bytes,
    uint32_t *out_underruns,
    uint64_t *out_total_submitted
);

/*
 * Set audio volume (0.0 to 1.0) and mute state (non-zero = muted).
 *
 * The setting applies to the next read or render, including frames already
 * queued. Volume and mute are preserved across reinitialization.
 */
xenora_status xenora_session_audio_set_volume(
    xenora_session session,
    float volume,
    uint32_t muted
);

/* Virtual Filesystem (Gate 6, #284) */
typedef uint64_t xenora_file;
#define XENORA_NO_FILE 0u

enum {
    XENORA_SEEK_SET = 0,
    XENORA_SEEK_CUR = 1,
    XENORA_SEEK_END = 2,
};

/*
 * Set the sandboxed root directory for the session virtual filesystem.
 */
xenora_status xenora_session_vfs_set_root(
    xenora_session session,
    const char *root_path
);

/*
 * Open a file within the virtual filesystem sandboxed root.
 * Missing files return XENORA_FILE_NOT_FOUND. Host permission failures return
 * XENORA_ACCESS_DENIED. Other host I/O failures return XENORA_IO_ERROR.
 */
xenora_status xenora_session_vfs_open(
    xenora_session session,
    const char *rel_path,
    uint32_t flags,
    xenora_file *out_file
);

/*
 * Read bytes from an open virtual file.
 */
xenora_status xenora_session_vfs_read(
    xenora_session session,
    xenora_file file,
    uint8_t *out_buffer,
    uint64_t count,
    uint64_t *out_bytes_read
);

/*
 * Seek within an open virtual file.
 */
xenora_status xenora_session_vfs_seek(
    xenora_session session,
    xenora_file file,
    int64_t offset,
    uint32_t whence,
    uint64_t *out_new_offset
);

/*
 * Close an open virtual file.
 */
xenora_status xenora_session_vfs_close(
    xenora_session session,
    xenora_file file
);

/*
 * Run the original audio and VFS PE fixture.
 */
xenora_status xenora_session_run_audio_vfs_fixture(
    xenora_session session,
    uint8_t *out_rgba,
    uint64_t capacity,
    uint32_t *out_width,
    uint32_t *out_height,
    uint32_t *out_audio_samples
);

/*
 * Read-only compatibility inspection of a caller-supplied PE file. Nothing in
 * `bytes` is executed: it is parsed and statically disassembled (bounded) only.
 * On XENORA_OK, XENORA_PE_ERROR or XENORA_INTERNAL_ERROR, `out_report` holds a
 * NUL-terminated UTF-8 report of `*out_required` bytes including the NUL.
 * XENORA_PE_ERROR means the file is not an acceptable PE32+ image;
 * XENORA_INTERNAL_ERROR means Xenora's own probe failed on it. The report says which. If `capacity`
 * is too small the call returns XENORA_BUFFER_TOO_SMALL with `*out_required`
 * set and nothing else written. Files over XENORA_MAX_INSPECT_BYTES return
 * XENORA_LIMIT_EXCEEDED.
 */
xenora_status xenora_inspect_executable(
    const uint8_t *bytes,
    uint64_t length,
    char *out_report,
    uint64_t capacity,
    uint64_t *out_required);

#endif
