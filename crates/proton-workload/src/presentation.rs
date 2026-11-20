use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresentationConfig {
    pub seconds: u32,
    #[serde(default)]
    pub resize: bool,
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub resize_cycles: u32,
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub stage_probe: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fragment_iterations: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cpu_rounds: u32,
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub latency_probe: bool,
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub runtime_feedback: bool,
    #[serde(default, skip_serializing_if = "crate::is_false")]
    pub frame_tags: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub latency_delay_ms: u32,
}

fn one() -> u32 {
    1
}
fn is_one(value: &u32) -> bool {
    *value == 1
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// Pixel of the input-reactive barcode. The fixed high word distinguishes the
/// owned fixture from boot surfaces, placeholders and unrelated images.
pub fn presentation_latency_pixel(x: u32, width: u32, sequence: u32, load_bit: u8) -> [u8; 4] {
    tag_pixel(x, width, 0xa5c3_0000 | (sequence & 0xffff), load_bit)
}

fn tag_pixel(x: u32, width: u32, word: u32, load_bit: u8) -> [u8; 4] {
    let bit = (u64::from(x) * 32 / u64::from(width.max(1))).min(31) as u32;
    let color = if word & (1 << bit) != 0 { 230 } else { 20 } + (load_bit & 1);
    [color, color, color, 255]
}

// Affine exponentiation computes the shader's wrapping recurrence without repeating thousands of GPU iterations for every CPU reference pixel.
fn load_transform(mut rounds: u32) -> (u32, u32) {
    let (mut a, mut c) = (1_u32, 0_u32);
    let (mut step_a, mut step_c) = (1_664_525_u32, 1_013_904_223_u32);
    while rounds != 0 {
        if rounds & 1 != 0 {
            c = c.wrapping_mul(step_a).wrapping_add(step_c);
            a = a.wrapping_mul(step_a);
        }
        step_c = step_c.wrapping_mul(step_a.wrapping_add(1));
        step_a = step_a.wrapping_mul(step_a);
        rounds >>= 1;
    }
    (a, c)
}

fn load_seed(x: u32, y: u32, width: u32, seed: u32, transform: (u32, u32)) -> u32 {
    let state = (seed ^ x.wrapping_add(y.wrapping_mul(width)))
        .wrapping_mul(transform.0)
        .wrapping_add(transform.1);
    state ^ (state >> 16)
}

#[derive(Debug, Serialize)]
pub struct FrameRate {
    pub average_fps: f64,
    /// Reciprocal of the mean duration of the slowest ceil(1%) of frames.
    pub one_percent_low_fps: f64,
}

/// Source render rate, including stalls.
pub fn frame_rate(sorted: &[u64], frequency: u64) -> Option<FrameRate> {
    if sorted.is_empty() || frequency == 0 || !sorted.is_sorted() {
        return None;
    }
    let total: u128 = sorted.iter().map(|&n| u128::from(n)).sum();
    let count = sorted.len().div_ceil(100);
    let tail: u128 = sorted[sorted.len() - count..]
        .iter()
        .map(|&n| u128::from(n))
        .sum();
    if total == 0 || tail == 0 {
        return None;
    }
    Some(FrameRate {
        average_fps: sorted.len() as f64 * frequency as f64 / total as f64,
        one_percent_low_fps: count as f64 * frequency as f64 / tail as f64,
    })
}

/// Disjoint source-frame stages.
pub fn frame_stage_ticks(boundaries: [u64; 9]) -> Option<[u64; 8]> {
    let mut stages = [0; 8];
    for (stage, pair) in stages.iter_mut().zip(boundaries.windows(2)) {
        *stage = pair[1].checked_sub(pair[0])?;
    }
    Some(stages)
}

/// Exact integer output of the two owned presentation pixel shaders.
pub fn presentation_pixel(x: u32, y: u32, width: u32, seed: u32) -> [u8; 4] {
    let r = x.wrapping_add(seed) as u8;
    let g = y.wrapping_add(seed >> 1) as u8;
    let b = if ((x / 64) ^ (y / 64) ^ (seed >> 7)) & 1 != 0 {
        192
    } else {
        32
    };
    if x >= width / 2 {
        [b, g, r, 255]
    } else {
        [r, g, b, 255]
    }
}

/// Validate every RGBA8 pixel, excluding D3D12 row-alignment padding.
pub fn verify_presentation(
    bytes: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    seed: u32,
) -> Option<u64> {
    verify_presentation_load(bytes, pitch, width, height, seed, 0)
}

/// Verify the optional shader load against an independent closed-form oracle.
pub fn verify_presentation_load(
    bytes: &[u8],
    pitch: usize,
    width: u32,
    height: u32,
    seed: u32,
    iterations: u32,
) -> Option<u64> {
    let row = usize::try_from(width).ok()?.checked_mul(4)?;
    let rows = usize::try_from(height).ok()?;
    if row == 0
        || rows == 0
        || pitch < row
        || bytes.len() < pitch.checked_mul(rows - 1)?.checked_add(row)?
    {
        return None;
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let latency = iterations & (1 << 31) != 0;
    let rounds = iterations & 0x3fff_ffff;
    let transform = load_transform(rounds);
    for y in 0..height {
        for (x, pixel) in bytes[y as usize * pitch..y as usize * pitch + row]
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
        {
            let load_bit = if rounds != 0 {
                (load_seed(x as u32, y, width, seed, transform) & 1) as u8
            } else {
                0
            };
            let mut expected = if latency {
                if iterations & (1 << 30) != 0 && y < 16 {
                    tag_pixel(x as u32, width, 0xb7d1_0000 | (seed >> 16), load_bit)
                } else {
                    presentation_latency_pixel(x as u32, width, seed, load_bit)
                }
            } else {
                presentation_pixel(x as u32, y, width, seed)
            };
            if !latency && rounds != 0 {
                let channel = if x as u32 >= width / 2 { 0 } else { 2 };
                expected[channel] += load_bit;
            }
            if *pixel != expected {
                return None;
            }
            for b in pixel {
                hash = (hash ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    Some(hash)
}

#[cfg(test)]
#[path = "../tests/unit/presentation.rs"]
mod tests;
