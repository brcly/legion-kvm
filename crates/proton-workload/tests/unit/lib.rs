use super::*;
#[test]
fn verification_ignores_padding_but_detects_stale_or_incorrect_pixels() {
    let mut image = [255, 0, 0, 255, 255, 0, 0, 255, 7, 7, 7, 7];
    assert!(verify_red(&image, 12, 2, 1).is_some());
    image[5] = 1;
    assert!(verify_red(&image, 12, 2, 1).is_none());
    assert!(verify_red(&image, 4, 2, 1).is_none());
    assert!(verify_red(&image, 12, 2, 2).is_none());
}
#[test]
fn final_row_does_not_require_trailing_alignment_padding() {
    let image = [255, 0, 0, 255, 7, 7, 7, 7, 255, 0, 0, 255];
    let tight = [255, 0, 0, 255, 255, 0, 0, 255];
    assert_eq!(verify_red(&image, 8, 1, 2), verify_red(&tight, 4, 1, 2));
    assert!(verify_red(&image[..11], 8, 1, 2).is_none());
    assert!(verify_red(&image, 0, 0, 0).is_none());
}
#[test]
fn cpu_result_depends_on_completed_work() {
    assert_eq!(cpu_work(10), cpu_work(10));
    assert_ne!(cpu_work(10), cpu_work(11));
}
#[test]
fn stage_ticks_use_the_reported_clock_and_reject_missing_samples() {
    let summary = timing_summary(&mut [40, 10, 30, 20], 1_000_000).expect("valid clock");
    assert_eq!(summary.mean_us, 25.0);
    assert_eq!(summary.p50_us, 20.0);
    assert!(timing_summary(&mut [], 1_000_000).is_none());
    assert!(timing_summary(&mut [10], 0).is_none());
}
#[test]
fn ordinary_configuration_preserves_compatibility_and_never_enables_queue_kicks() {
    let config: Config = serde_json::from_str(r#"{"schema":1,"width":1920,"height":1080,"warmup_ms":500,"sample_ms":2000,"cpu_rounds":100000,"gpu_clears":64}"#).expect("original configuration");
    assert!(config.valid());
    assert!(!config.diagnostic_completion_kick);
    assert!(!config.gpu_timestamps);
    assert!(!config.diagnostic_event_trace);
    let wire = serde_json::to_value(config).expect("serializable configuration");
    assert!(wire.get("diagnostic_completion_kick").is_none());
    assert!(wire.get("gpu_timestamps").is_none());
    assert!(wire.get("diagnostic_event_trace").is_none());
    assert!(wire.get("diagnostic_cpu_profile").is_none());
    assert!(wire.get("pipeline").is_none());
    assert!(wire.get("presentation").is_none());
}
#[test]
fn presentation_load_preserves_defaults_and_bounds_both_work_dimensions() {
    let mut config: Config = serde_json::from_str(r#"{"schema":1,"width":1280,"height":720,"warmup_ms":200,"sample_ms":1000,"cpu_rounds":100000,"gpu_clears":64,"presentation":{"seconds":20}}"#).expect("original configuration");
    let wire = serde_json::to_value(&config).expect("serializable configuration");
    assert!(wire["presentation"].get("fragment_iterations").is_none());
    assert!(wire["presentation"].get("cpu_rounds").is_none());
    for (shader, cpu, valid) in [
        (0, 0, true),
        (8192, 1_000_000, true),
        (8193, 0, false),
        (0, 1_000_001, false),
        (u32::MAX, 0, false),
        (0, u32::MAX, false),
    ] {
        let settings = config.presentation.as_mut().expect("presentation");
        settings.fragment_iterations = shader;
        settings.cpu_rounds = cpu;
        assert_eq!(config.valid(), valid);
    }
}

#[test]
fn windowed_fixture_is_bounded_and_separate_from_offscreen_diagnostics() {
    let base = r#"{"schema":1,"width":1280,"height":720,"warmup_ms":200,"sample_ms":1000,"cpu_rounds":100000,"gpu_clears":64,"presentation":{"seconds":20,"resize":true}}"#;
    let mut config: Config = serde_json::from_str(base).expect("windowed configuration");
    assert!(config.valid());
    for seconds in [0, 14, 31, u32::MAX] {
        config
            .presentation
            .as_mut()
            .expect("presentation configured")
            .seconds = seconds;
        assert!(!config.valid());
    }
    config
        .presentation
        .as_mut()
        .expect("presentation configured")
        .seconds = 20;
    config.diagnostic_completion_kick = true;
    assert!(!config.valid());
    config.diagnostic_completion_kick = false;
    config.diagnostic_cpu_profile = true;
    assert!(!config.valid());
    config.diagnostic_cpu_profile = false;
    config.pipeline = Some(PipelineConfig {
        shader_rounds: 1,
        draws_per_frame: 1,
        command_stage_probe: false,
        recording_probe: false,
        retirement_probe: false,
    });
    assert!(!config.valid());
}
#[test]
fn pipeline_requires_real_timestamps_and_rejects_out_of_range_work_or_queue_kicks() {
    let mut config: Config = serde_json::from_str(r#"{"schema":1,"width":1920,"height":1080,"warmup_ms":100,"sample_ms":500,"cpu_rounds":100000,"gpu_clears":64,"gpu_timestamps":true,"pipeline":{"shader_rounds":32}}"#).expect("shader configuration");
    assert!(config.valid());
    config.gpu_timestamps = false;
    assert!(!config.valid());
    config.gpu_timestamps = true;
    config.diagnostic_completion_kick = true;
    assert!(!config.valid());
    config.diagnostic_completion_kick = false;
    for rounds in [0, 257] {
        config.pipeline = Some(PipelineConfig {
            shader_rounds: rounds,
            draws_per_frame: 1,
            command_stage_probe: false,
            recording_probe: false,
            retirement_probe: false,
        });
        assert!(!config.valid());
    }
}

#[test]
fn retirement_control_requires_command_stage_timing() {
    let mut config: Config = serde_json::from_str(r#"{"schema":1,"width":1920,"height":1080,"warmup_ms":100,"sample_ms":500,"cpu_rounds":100000,"gpu_clears":64,"gpu_timestamps":true,"pipeline":{"shader_rounds":32,"retirement_probe":true}}"#).expect("diagnostic configuration");
    assert!(!config.valid());
    config
        .pipeline
        .as_mut()
        .expect("pipeline configured")
        .command_stage_probe = true;
    assert!(config.valid());
}

#[test]
fn source_frame_tags_require_runtime_feedback_and_input_mode() {
    let mut config: Config = serde_json::from_str(r#"{"schema":1,"width":1920,"height":1080,"warmup_ms":200,"sample_ms":1000,"cpu_rounds":25000,"gpu_clears":1,"presentation":{"seconds":25,"frame_tags":true}}"#).expect("frame tag configuration");
    assert!(!config.valid());
    let settings = config.presentation.as_mut().expect("presentation");
    settings.latency_probe = true;
    assert!(!config.valid());
    config
        .presentation
        .as_mut()
        .expect("presentation")
        .runtime_feedback = true;
    assert!(config.valid());
}

#[test]
fn repeated_resize_configuration_is_bounded_and_requires_resize_mode() {
    let mut value = serde_json::json!({"schema":1,"width":1920,"height":1080,
        "warmup_ms":200,"sample_ms":1000,"cpu_rounds":25000,"gpu_clears":1,"presentation":{"seconds":30,"resize":true,"resize_cycles":48}});
    let valid: Config =
        serde_json::from_value(value.clone()).expect("bounded stress configuration");
    assert!(valid.valid());
    for cycles in [0, 49, u32::MAX] {
        value["presentation"]["resize_cycles"] = cycles.into();
        let invalid: Config = serde_json::from_value(value.clone()).expect("numeric configuration");
        assert!(!invalid.valid());
    }
    value["presentation"]["resize_cycles"] = 2.into();
    value["presentation"]["resize"] = false.into();
    let invalid: Config = serde_json::from_value(value).expect("numeric configuration");
    assert!(!invalid.valid());
}

#[test]
fn long_shader_load_is_confined_to_bounded_latency_diagnostics() {
    let mut config: Config = serde_json::from_str(r#"{"schema":1,"width":1920,"height":1080,"warmup_ms":200,"sample_ms":1000,"cpu_rounds":25000,"gpu_clears":1,"presentation":{"seconds":25,"latency_probe":true,"fragment_iterations":65536}}"#).expect("explicit long-shader diagnostic");
    assert!(config.valid());
    config
        .presentation
        .as_mut()
        .expect("presentation")
        .fragment_iterations = 65537;
    assert!(!config.valid());
    let settings = config.presentation.as_mut().expect("presentation");
    settings.fragment_iterations = 65536;
    settings.latency_probe = false;
    assert!(!config.valid());
}

#[test]
fn runtime_feedback_requires_the_explicit_input_reactive_fixture() {
    let mut config: Config = serde_json::from_str(r#"{"schema":1,"width":1920,"height":1080,"warmup_ms":200,"sample_ms":1000,"cpu_rounds":25000,"gpu_clears":1,"presentation":{"seconds":25,"runtime_feedback":true}}"#).expect("feedback configuration");
    assert!(!config.valid());
    config
        .presentation
        .as_mut()
        .expect("presentation")
        .latency_probe = true;
    assert!(config.valid());
}
