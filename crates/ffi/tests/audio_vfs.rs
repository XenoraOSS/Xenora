// SPDX-License-Identifier: GPL-3.0-or-later

//! Gate 6 acceptance on a native Metal device: audio PCM streaming and
//! sandboxed virtual filesystem (VFS) integration through the C interface (#284).
//! Needs `--features gpu-tests` (scripts/test-gpu.sh).
#![cfg(feature = "gpu-tests")]

use std::sync::Mutex;
use xenora_ffi::*;

/// Tests share the process-wide session table and one GPU; run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn lock_serial() -> std::sync::MutexGuard<'static, ()> {
    SERIAL.lock().unwrap_or_else(|e| e.into_inner())
}

fn session() -> u64 {
    let mut handle = 0;
    assert_eq!(
        unsafe { xenora_session_create(XENORA_ABI_VERSION, &mut handle) },
        XENORA_OK
    );
    assert_ne!(handle, 0);
    handle
}

#[test]
fn audio_pcm_queue_streaming_ordering_and_volume() {
    let _guard = lock_serial();
    let sess = session();

    // Rejection on invalid arguments
    assert_eq!(
        xenora_session_audio_init(sess, 0, 2, XENORA_AUDIO_FORMAT_PCM_S16LE),
        XENORA_INVALID_ARGUMENT
    );
    assert_eq!(
        xenora_session_audio_init(0xdead_beef, 48000, 2, XENORA_AUDIO_FORMAT_PCM_S16LE),
        XENORA_INVALID_HANDLE
    );

    // Initialize 48000 Hz stereo 16-bit PCM
    assert_eq!(
        xenora_session_audio_init(sess, 48000, 2, XENORA_AUDIO_FORMAT_PCM_S16LE),
        XENORA_OK
    );

    // Initial status should be empty
    let (mut queued, mut underruns, mut total) = (0u32, 0u32, 0u64);
    assert_eq!(
        unsafe { xenora_session_audio_status(sess, &mut queued, &mut underruns, &mut total) },
        XENORA_OK
    );
    assert_eq!(queued, 0);
    assert_eq!(underruns, 0);
    assert_eq!(total, 0);

    // Generate known PCM samples: 100 stereo samples (400 bytes)
    let mut pcm = Vec::with_capacity(400);
    for i in 0..100i16 {
        let left = i * 100;
        let right = -left;
        pcm.extend_from_slice(&left.to_le_bytes());
        pcm.extend_from_slice(&right.to_le_bytes());
    }

    assert_eq!(
        unsafe { xenora_session_audio_submit(sess, pcm.as_ptr(), pcm.len() as u64) },
        XENORA_OK
    );

    assert_eq!(
        unsafe { xenora_session_audio_status(sess, &mut queued, &mut underruns, &mut total) },
        XENORA_OK
    );
    assert_eq!(queued, 400);
    assert_eq!(total, 400);
    assert_eq!(underruns, 0);

    // Read back first 50 samples (200 bytes)
    let mut read_buf = vec![0u8; 200];
    let mut bytes_read = 0u64;
    assert_eq!(
        unsafe { xenora_session_audio_read(sess, read_buf.as_mut_ptr(), 200, &mut bytes_read) },
        XENORA_OK
    );
    assert_eq!(bytes_read, 200);
    assert_eq!(&read_buf[..], &pcm[..200]);

    // Volume attenuation: set to 0.5
    assert_eq!(xenora_session_audio_set_volume(sess, 0.5, 0), XENORA_OK);

    // Submit 2 stereo samples (8 bytes): 2000, -2000, 4000, -4000
    let mut half_vol_in = Vec::new();
    half_vol_in.extend_from_slice(&2000i16.to_le_bytes());
    half_vol_in.extend_from_slice(&(-2000i16).to_le_bytes());
    half_vol_in.extend_from_slice(&4000i16.to_le_bytes());
    half_vol_in.extend_from_slice(&(-4000i16).to_le_bytes());

    assert_eq!(
        unsafe {
            xenora_session_audio_submit(sess, half_vol_in.as_ptr(), half_vol_in.len() as u64)
        },
        XENORA_OK
    );

    // The remaining frames were queued before the volume change. They
    // still drain first, now at half volume.
    let mut drain = vec![0u8; 200];
    assert_eq!(
        unsafe { xenora_session_audio_read(sess, drain.as_mut_ptr(), 200, &mut bytes_read) },
        XENORA_OK
    );
    assert_eq!(bytes_read, 200);
    let expected: Vec<u8> = (50..100i16)
        .flat_map(|i| [i * 50, -i * 50])
        .flat_map(i16::to_le_bytes)
        .collect();
    assert_eq!(drain, expected);

    // Now read the 8 bytes attenuated samples
    let mut attenuated = vec![0u8; 8];
    assert_eq!(
        unsafe { xenora_session_audio_read(sess, attenuated.as_mut_ptr(), 8, &mut bytes_read) },
        XENORA_OK
    );
    assert_eq!(bytes_read, 8);
    let s0 = i16::from_le_bytes([attenuated[0], attenuated[1]]);
    let s1 = i16::from_le_bytes([attenuated[2], attenuated[3]]);
    let s2 = i16::from_le_bytes([attenuated[4], attenuated[5]]);
    let s3 = i16::from_le_bytes([attenuated[6], attenuated[7]]);
    assert_eq!(s0, 1000);
    assert_eq!(s1, -1000);
    assert_eq!(s2, 2000);
    assert_eq!(s3, -2000);

    // Test mute (muted = 1)
    assert_eq!(xenora_session_audio_set_volume(sess, 1.0, 1), XENORA_OK);
    assert_eq!(
        unsafe {
            xenora_session_audio_submit(sess, half_vol_in.as_ptr(), half_vol_in.len() as u64)
        },
        XENORA_OK
    );
    let mut muted_out = vec![0u8; 8];
    assert_eq!(
        unsafe { xenora_session_audio_read(sess, muted_out.as_mut_ptr(), 8, &mut bytes_read) },
        XENORA_OK
    );
    assert_eq!(muted_out, vec![0u8; 8]);

    xenora_session_destroy(sess);
}

#[test]
fn virtual_filesystem_sandboxing_and_traversal_rejection() {
    let _guard = lock_serial();
    let sess = session();

    let mut file = 0u64;

    // Reject non-existent root
    let bad_root = std::ffi::CString::new("/nonexistent_dir_xenora").unwrap();
    assert_eq!(
        unsafe { xenora_session_vfs_set_root(sess, bad_root.as_ptr()) },
        XENORA_INVALID_ARGUMENT
    );

    // Set valid root
    let root_path = if std::path::Path::new("fixtures/assets").exists() {
        "fixtures/assets"
    } else {
        "../../fixtures/assets"
    };
    let c_root = std::ffi::CString::new(root_path).unwrap();
    assert_eq!(
        unsafe { xenora_session_vfs_set_root(sess, c_root.as_ptr()) },
        XENORA_OK
    );

    // Traversal attacks rejected
    for bad in &[
        "../escape.txt",
        "../../etc/passwd",
        "/etc/passwd",
        "sub/../../secret",
        "",
    ] {
        let bad_c = std::ffi::CString::new(*bad).unwrap();
        assert_eq!(
            unsafe { xenora_session_vfs_open(sess, bad_c.as_ptr(), 0, &mut file) },
            XENORA_INVALID_ARGUMENT,
            "traversal attempt '{}' was not rejected",
            bad
        );
    }

    let missing = std::ffi::CString::new("missing-xenora-test-asset.bin").unwrap();
    assert_eq!(
        unsafe { xenora_session_vfs_open(sess, missing.as_ptr(), 0, &mut file) },
        XENORA_FILE_NOT_FOUND
    );
    assert_eq!(file, XENORA_NO_FILE);

    // Open existing valid asset
    let good = std::ffi::CString::new("asset.bin").unwrap();
    assert_eq!(
        unsafe { xenora_session_vfs_open(sess, good.as_ptr(), 0, &mut file) },
        XENORA_OK
    );
    assert_ne!(file, XENORA_NO_FILE);

    // Read header (20 bytes: 4B magic + 4B ver + 4B width + 4B height + 4B audio_samples)
    let mut header = [0u8; 20];
    let mut read_bytes = 0u64;
    assert_eq!(
        unsafe { xenora_session_vfs_read(sess, file, header.as_mut_ptr(), 20, &mut read_bytes) },
        XENORA_OK
    );
    assert_eq!(read_bytes, 20);
    assert_eq!(&header[0..4], b"XAST");
    let version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    assert_eq!(version, 1);
    let width = u32::from_le_bytes([header[8], header[9], header[10], header[11]]);
    assert_eq!(width, 2);
    let height = u32::from_le_bytes([header[12], header[13], header[14], header[15]]);
    assert_eq!(height, 2);
    let audio_samples = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    assert_eq!(audio_samples, 256);

    // Seek to audio offset (36) using SEEK_SET
    let mut new_pos = 0u64;
    assert_eq!(
        unsafe { xenora_session_vfs_seek(sess, file, 36, XENORA_SEEK_SET, &mut new_pos) },
        XENORA_OK
    );
    assert_eq!(new_pos, 36);

    // Read 4 bytes of audio
    let mut sample_bytes = [0u8; 4];
    assert_eq!(
        unsafe {
            xenora_session_vfs_read(sess, file, sample_bytes.as_mut_ptr(), 4, &mut read_bytes)
        },
        XENORA_OK
    );
    assert_eq!(read_bytes, 4);

    // Seek using SEEK_END
    assert_eq!(
        unsafe { xenora_session_vfs_seek(sess, file, 0, XENORA_SEEK_END, &mut new_pos) },
        XENORA_OK
    );
    assert_eq!(new_pos, 20 + 16 + 512); // total asset size = 548 bytes

    // Close file
    assert_eq!(xenora_session_vfs_close(sess, file), XENORA_OK);

    // Double close rejected
    assert_eq!(xenora_session_vfs_close(sess, file), XENORA_INVALID_HANDLE);

    xenora_session_destroy(sess);
}

#[test]
fn audio_vfs_guest_fixture_executes_asset_load_and_pcm_stream() {
    let _guard = lock_serial();
    let sess = session();

    let mut out_rgba = [0u8; 16];
    let mut out_w = 0u32;
    let mut out_h = 0u32;
    let mut out_samples = 0u32;

    assert_eq!(
        unsafe {
            xenora_session_run_audio_vfs_fixture(
                sess,
                out_rgba.as_mut_ptr(),
                16,
                &mut out_w,
                &mut out_h,
                &mut out_samples,
            )
        },
        XENORA_OK
    );

    assert_eq!(out_w, 2);
    assert_eq!(out_h, 2);
    assert_eq!(out_samples, 256);

    // Expected texture from asset.bin: 2x2 RGBA8:
    // (0,0): Orange [255, 128, 0, 255]
    // (1,0): Purple [128, 0, 255, 255]
    // (0,1): Teal   [0, 255, 128, 255]
    // (1,1): Pink   [255, 0, 128, 255]
    let expected_rgba: [u8; 16] = [
        255, 128, 0, 255, 128, 0, 255, 255, 0, 255, 128, 255, 255, 0, 128, 255,
    ];
    assert_eq!(
        out_rgba, expected_rgba,
        "Metal rendered frame did not match asset texture"
    );

    // Audio status: guest submitted 256 samples (512 bytes)
    let (mut queued, mut underruns, mut total) = (0u32, 0u32, 0u64);
    assert_eq!(
        unsafe { xenora_session_audio_status(sess, &mut queued, &mut underruns, &mut total) },
        XENORA_OK
    );
    assert_eq!(queued, 512);
    assert_eq!(total, 512);
    assert_eq!(underruns, 0);

    // Read back submitted audio samples and verify
    let mut pcm_buf = vec![0u8; 512];
    let mut read_bytes = 0u64;
    assert_eq!(
        unsafe { xenora_session_audio_read(sess, pcm_buf.as_mut_ptr(), 512, &mut read_bytes) },
        XENORA_OK
    );
    assert_eq!(read_bytes, 512);

    // The first sample in asset.bin sine wave is 0
    let s0 = i16::from_le_bytes([pcm_buf[0], pcm_buf[1]]);
    assert_eq!(s0, 0);

    xenora_session_destroy(sess);
}

#[test]
fn audio_vfs_rejection_of_invalid_and_stale_handles() {
    let _guard = lock_serial();
    let dead_sess = 0xdead_beefu64;

    let mut queued = 0u32;
    let mut underruns = 0u32;
    let mut total = 0u64;
    assert_eq!(
        unsafe { xenora_session_audio_status(dead_sess, &mut queued, &mut underruns, &mut total) },
        XENORA_INVALID_HANDLE
    );

    let mut file = 0u64;
    let path = std::ffi::CString::new("asset.bin").unwrap();
    assert_eq!(
        unsafe { xenora_session_vfs_open(dead_sess, path.as_ptr(), 0, &mut file) },
        XENORA_INVALID_HANDLE
    );

    let sess = session();
    // Null pointer checks
    assert_eq!(
        unsafe { xenora_session_vfs_open(sess, std::ptr::null(), 0, &mut file) },
        XENORA_INVALID_ARGUMENT
    );
    assert_eq!(
        unsafe { xenora_session_vfs_open(sess, path.as_ptr(), 0, std::ptr::null_mut()) },
        XENORA_INVALID_ARGUMENT
    );

    // Buffer too small for fixture
    let mut small_buf = [0u8; 8];
    let mut w = 0u32;
    let mut h = 0u32;
    let mut samples = 0u32;
    assert_eq!(
        unsafe {
            xenora_session_run_audio_vfs_fixture(
                sess,
                small_buf.as_mut_ptr(),
                8,
                &mut w,
                &mut h,
                &mut samples,
            )
        },
        XENORA_BUFFER_TOO_SMALL
    );

    xenora_session_destroy(sess);
}
