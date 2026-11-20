#![no_std]
//! Portable, deterministic input and correctness checks for the Windows probe.
extern crate alloc;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

mod gpu_timing;
pub use gpu_timing::{GpuClockCalibration, GpuObservation, GpuTiming, summarize_gpu};
mod presentation;
mod rendering;
pub use presentation::{
    FrameRate, PresentationConfig, frame_rate, frame_stage_ticks, presentation_pixel,
    verify_presentation, verify_presentation_load,
};
pub use rendering::{
    PipelineConfig, PipelineTiming, RenderImageCheck, shader_pixel, verify_shader,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: u32,
    pub width: u32,
    pub height: u32,
    pub warmup_ms: u32,
    pub sample_ms: u32,
    pub cpu_rounds: u32,
    pub gpu_clears: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub diagnostic_completion_kick: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub gpu_timestamps: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub diagnostic_event_trace: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub diagnostic_cpu_profile: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<PipelineConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presentation: Option<PresentationConfig>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Config {
    pub fn valid(&self) -> bool {
        self.schema == 1
            && (64..=3840).contains(&self.width)
            && (64..=2160).contains(&self.height)
            && (100..=5000).contains(&self.warmup_ms)
            && (500..=10000).contains(&self.sample_ms)
            && (1..=1_000_000).contains(&self.cpu_rounds)
            && (1..=4096).contains(&self.gpu_clears)
            && (!self.diagnostic_event_trace || self.gpu_timestamps)
            && (!self.diagnostic_cpu_profile || self.pipeline.is_some())
            && self.presentation.as_ref().is_none_or(|p| {
                (15..=30).contains(&p.seconds)
                    && p.fragment_iterations <= if p.latency_probe { 65536 } else { 8192 }
                    && (1..=48).contains(&p.resize_cycles)
                    && (p.resize || p.resize_cycles == 1)
                    && p.cpu_rounds <= 1_000_000
                    && (!p.runtime_feedback || p.latency_probe)
                    && (!p.frame_tags || p.runtime_feedback)
                    && p.latency_delay_ms <= 100
                    && (p.latency_probe || p.latency_delay_ms == 0)
                    && (!p.latency_probe || (!p.resize && !p.stage_probe))
                    && self.pipeline.is_none()
                    && !self.diagnostic_cpu_profile
                    && !self.diagnostic_event_trace
                    && !self.diagnostic_completion_kick
            })
            && self.pipeline.as_ref().is_none_or(|p| {
                (1..=256).contains(&p.shader_rounds)
                    && (1..=64).contains(&p.draws_per_frame)
                    && (!p.retirement_probe || p.command_stage_probe)
                    && self.gpu_timestamps
                    && !self.diagnostic_completion_kick
            })
    }
}

/// Integer dependency chains with explicit wrapping, independent of CPUID/ISA.
#[inline(never)]
pub fn cpu_work(rounds: u32) -> u64 {
    let mut a = 0x1234_5678_9abc_def0_u64;
    let mut b = 0xfedc_ba98_7654_3210_u64;
    for i in 0..rounds {
        a = a.wrapping_add(b).rotate_left(17) ^ u64::from(i);
        b = b.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(a);
        core::hint::black_box((a, b));
    }
    a ^ b
}

/// Validate every texel while excluding D3D row alignment padding.
pub fn verify_red(bytes: &[u8], row_pitch: usize, width: usize, height: usize) -> Option<u64> {
    let row_size = width.checked_mul(4)?;
    if width == 0 || height == 0 || row_size > row_pitch {
        return None;
    }
    // D3D12 footprints need not include alignment padding after the last row.
    let required = (height - 1).checked_mul(row_pitch)?.checked_add(row_size)?;
    if bytes.len() < required {
        return None;
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for row in 0..height {
        let start = row * row_pitch;
        for rgba in bytes[start..start + row_size].as_chunks::<4>().0 {
            if *rgba != [255, 0, 0, 255] {
                return None;
            }
            for &byte in rgba {
                hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
            }
        }
    }
    Some(hash)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sample {
    pub mode: alloc::string::String,
    pub iterations: u64,
    pub elapsed_ms: f64,
    pub iterations_per_second: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
    pub cpu_checksum: u64,
    #[serde(default)]
    pub stages: alloc::collections::BTreeMap<alloc::string::String, Timing>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<GpuTiming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<PipelineTiming>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Timing {
    pub mean_us: f64,
    pub p50_us: f64,
    pub p95_us: f64,
    pub p99_us: f64,
}

/// Summarize QPC ticks after timing; reject empty samples or an invalid clock.
pub fn timing_summary(ticks: &mut [u64], frequency: u64) -> Option<Timing> {
    if ticks.is_empty() || frequency == 0 {
        return None;
    }
    ticks.sort_unstable();
    let us = |tick: u64| tick as f64 * 1_000_000.0 / frequency as f64;
    let sum: u128 = ticks.iter().map(|&tick| u128::from(tick)).sum();
    Some(Timing {
        mean_us: sum as f64 / ticks.len() as f64 * 1_000_000.0 / frequency as f64,
        p50_us: us(percentile(ticks, 50)),
        p95_us: us(percentile(ticks, 95)),
        p99_us: us(percentile(ticks, 99)),
    })
}

pub fn percentile(sorted: &[u64], numerator: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) * numerator / 100).min(sorted.len() - 1)]
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Report {
    pub schema: u32,
    pub passed: bool,
    pub gpu_verified: bool,
    pub workload: alloc::string::String,
    pub adapter: alloc::string::String,
    pub vendor_id: u32,
    pub device_id: u32,
    pub logical_cpus: u32,
    pub xcr0: u64,
    #[serde(default)]
    pub qpc_frequency: u64,
    #[serde(default)]
    pub diagnostic_completion_kicks: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timestamp_validation_copy_ticks: Vec<u64>,
    pub config: Config,
    pub expected_cpu_checksum: u64,
    pub image_hash_before: u64,
    pub image_hash_after: u64,
    pub cases: Vec<Sample>,
}

#[cfg(test)]
#[path = "../tests/unit/lib.rs"]
mod tests;
