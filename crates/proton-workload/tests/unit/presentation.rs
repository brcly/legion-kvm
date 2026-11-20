use super::*;
use alloc::vec::Vec;

#[test]
fn frame_tag_strip_oracle_requires_exact_frame_and_input_generation() {
    let (width, height, seed) = (64_u32, 32_u32, (123_u32 << 16) | 7);
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let pixel = if y < 16 {
                tag_pixel(x, width, 0xb7d1_0000 | (seed >> 16), 0)
            } else {
                presentation_latency_pixel(x, width, seed, 0)
            };
            bytes.extend_from_slice(&pixel);
        }
    }
    let mode = (1 << 31) | (1 << 30);
    assert!(verify_presentation_load(&bytes, 256, width, height, seed, mode).is_some());
    for wrong in [seed ^ 1, seed ^ (1 << 16)] {
        assert!(verify_presentation_load(&bytes, 256, width, height, wrong, mode).is_none());
    }
    bytes[0] ^= 1;
    assert!(verify_presentation_load(&bytes, 256, width, height, seed, mode).is_none());
}

#[test]
fn latency_barcode_has_fixed_identity_and_changes_only_with_sequence() {
    for sequence in [0, 1, 2, 40, 0x1234, 0xffff] {
        let mut decoded = 0_u32;
        for bit in 0..32 {
            let x = (bit * 2 + 1) * 1920 / 64;
            let pixel = presentation_latency_pixel(x, 1920, sequence, 1);
            assert_eq!(pixel[0], pixel[1]);
            assert_eq!(pixel[1], pixel[2]);
            decoded |= u32::from(pixel[0] > 128) << bit;
        }
        assert_eq!(decoded, 0xa5c3_0000 | sequence);
    }
}

#[test]
fn latency_image_oracle_rejects_wrong_sequence_and_pixel_corruption() {
    let (width, height, sequence) = (64_u32, 2_u32, 17_u32);
    let mut bytes = Vec::new();
    for _ in 0..height {
        for x in 0..width {
            bytes.extend_from_slice(&presentation_latency_pixel(x, width, sequence, 0));
        }
    }
    assert!(verify_presentation_load(&bytes, 256, width, height, sequence, 1 << 31).is_some());
    assert!(verify_presentation_load(&bytes, 256, width, height, sequence + 1, 1 << 31).is_none());
    bytes[23] ^= 1;
    assert!(verify_presentation_load(&bytes, 256, width, height, sequence, 1 << 31).is_none());
}
#[test]
fn closed_form_load_matches_naive_wrapping_iterations_and_high_bits() {
    for rounds in [1, 2, 3, 31, 512, 8192, 65536] {
        let transform = load_transform(rounds);
        for seed in [0, u32::MAX, 73] {
            for (x, y, width) in [(0, 0, 64), (63, 17, 64), (1919, 1079, 1920)] {
                let mut state = seed ^ (x + y * width);
                for _ in 0..rounds {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                }
                assert_eq!(
                    load_seed(x, y, width, seed, transform),
                    state ^ (state >> 16)
                );
            }
        }
    }
    assert_ne!(
        load_seed(1, 1, 64, 73, load_transform(8192)),
        load_seed(1, 1, 64, 73, load_transform(512))
    );
}

#[test]
fn loaded_image_oracle_rejects_wrong_cost_and_any_corrupted_pixel() {
    let mut data = [0_u8; 32];
    for y in 0..2_u32 {
        for x in 0..4_u32 {
            let mut seed = 73 ^ (x + y * 4);
            for _ in 0..31 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            }
            let mut pixel = presentation_pixel(x, y, 4, 73);
            pixel[if x >= 2 { 0 } else { 2 }] += ((seed ^ (seed >> 16)) & 1) as u8;
            let offset = (y * 16 + x * 4) as usize;
            data[offset..offset + 4].copy_from_slice(&pixel);
        }
    }
    assert!(verify_presentation_load(&data, 16, 4, 2, 73, 31).is_some());
    assert!(verify_presentation_load(&data, 16, 4, 2, 73, 30).is_none());
    data[19] ^= 1;
    assert!(verify_presentation_load(&data, 16, 4, 2, 73, 31).is_none());
}
#[test]
fn fps_includes_stalls_and_uses_the_slowest_one_percent_mean() {
    let mut ticks = [10_u64; 101];
    ticks[99] = 20;
    ticks[100] = 180;
    let rate = frame_rate(&ticks, 1000).expect("valid frame times");
    assert_eq!(rate.average_fps, 101000.0 / 1190.0);
    assert_eq!(rate.one_percent_low_fps, 10.0);
    assert!(frame_rate(&[], 1000).is_none());
    assert!(frame_rate(&[0, 0], 1000).is_none());
    assert!(frame_rate(&[10], 0).is_none());
    assert!(frame_rate(&[20, 10], 1000).is_none());
    let stages = frame_stage_ticks([0, 2, 3, 5, 8, 13, 21, 34, 55]).expect("monotonic stamps");
    assert_eq!(stages.iter().sum::<u64>(), 55);
    assert!(frame_stage_ticks([0, 2, 3, 5, 4, 13, 21, 34, 55]).is_none());
}
#[test]
fn oracle_checks_both_pipeline_outputs_seed_and_alignment_padding() {
    let mut data = alloc::vec![7;40];
    for y in 0..2 {
        for x in 0..4 {
            let at = y * 24 + x * 4;
            data[at..at + 4].copy_from_slice(&presentation_pixel(x as u32, y as u32, 4, 73));
        }
    }
    assert!(verify_presentation(&data, 24, 4, 2, 73).is_some());
    assert!(verify_presentation(&data, 24, 4, 2, 74).is_none());
    data[32] ^= 1;
    assert!(verify_presentation(&data, 24, 4, 2, 73).is_none());
    assert!(verify_presentation(&data[..39], 24, 4, 2, 73).is_none());
}
