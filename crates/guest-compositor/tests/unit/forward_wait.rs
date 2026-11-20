use super::*;
use std::{
    io::Write,
    os::{fd::AsFd, unix::net::UnixStream},
};

#[test]
fn acquire_eventfd_wakes_without_wayland_or_drm_activity_and_is_not_consumed() {
    let (read, _write) = UnixStream::pair().expect("idle display socket");
    let acquire = rustix::event::eventfd(
        0,
        rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK,
    )
    .expect("owned acquire eventfd");
    rustix::io::write(&acquire, &1_u64.to_ne_bytes()).expect("signal acquire");
    let mut waiter = ForwardWait::new(&json!({"forward_wait":"events"})).expect("KMS event mode");
    let wakes = waiter
        .wait_with_acquires([read.as_fd()], std::iter::once(acquire.as_fd()))
        .expect("wait on all live sources");
    assert_eq!(wakes, 1);
    let report = waiter.report().expect("report");
    assert_eq!(report["wait_calls"], 1);
    assert_eq!(report["timeouts"], 0);
    let mut counter = [0_u8; 8];
    rustix::io::read(&acquire, &mut counter).expect("fence owner can still observe readiness");
    assert_eq!(u64::from_ne_bytes(counter), 1);
}

#[test]
fn acquire_wait_retains_idle_timeout_and_rejects_disconnected_extra_fd() {
    let (read, _write) = UnixStream::pair().expect("idle display socket");
    let (extra, peer) = UnixStream::pair().expect("owned extra event source");
    let mut waiter = ForwardWait::new(&json!({"forward_wait":"events"})).expect("KMS event mode");
    assert_eq!(
        waiter
            .wait_with_acquires([read.as_fd()], std::iter::once(extra.as_fd()))
            .expect("bounded idle timeout"),
        0
    );
    assert_eq!(waiter.report().expect("report")["timeouts"], 1);
    drop(peer);
    assert!(
        waiter
            .wait_with_acquires([read.as_fd()], std::iter::once(extra.as_fd()))
            .is_err()
    );
}

#[test]
fn unreadable_socket_times_out_but_readable_socket_wakes_the_wait() {
    let (read, mut write) = UnixStream::pair().expect("owned sockets");
    let mut waiter = ForwardWait::new(&json!({"forward_wait":"events"})).expect("event mode");
    waiter
        .wait([read.as_fd()], true)
        .expect("bounded fence retry");
    write.write_all(b"ready").expect("publish event");
    waiter
        .wait([read.as_fd()], false)
        .expect("immediate socket readiness");
    let report = waiter.report().expect("CPU clock");
    assert_eq!(report["wait_calls"], 2);
    assert_eq!(report["timeouts"], 1);
}

#[test]
fn mode_defaults_to_spin_and_rejects_invalid_settings() {
    let mut waiter = ForwardWait::new(&json!({})).expect("original mode");
    waiter.wait([], false).expect("original yield");
    assert_eq!(waiter.report().expect("CPU clock")["wait_calls"], 0);
    for value in [json!(null), json!(true), json!("unknown")] {
        assert!(ForwardWait::new(&json!({"forward_wait":value})).is_err());
    }
    for value in [
        json!(null),
        json!(true),
        json!(99),
        json!(1001),
        json!("100"),
    ] {
        assert!(ForwardWait::new(&json!({"forward_idle_retry_us":value})).is_err());
    }
    for retry in [100, 250, 1000] {
        let waiter =
            ForwardWait::new(&json!({"forward_wait":"events", "forward_idle_retry_us":retry}))
                .expect("bounded retry");
        assert_eq!(waiter.report().expect("report")["idle_retry_us"], retry);
    }
}

#[test]
fn hybrid_yields_for_pending_gpu_work_but_waits_for_idle_socket_events() {
    let mut waiter = ForwardWait::new(&json!({"forward_wait":"hybrid"})).expect("hybrid mode");
    waiter
        .wait([], true)
        .expect("GPU completion stays immediately queryable");
    assert!(
        waiter.wait([], false).is_err(),
        "idle waits need actual event descriptors"
    );
    let (read, mut write) = UnixStream::pair().expect("owned sockets");
    waiter
        .wait([read.as_fd()], false)
        .expect("bounded idle retry");
    write.write_all(b"ready").expect("publish socket event");
    waiter
        .wait([read.as_fd()], false)
        .expect("socket interrupts idle wait");
    let report = waiter.report().expect("CPU clock");
    assert_eq!(report["mode"], "hybrid");
    assert_eq!(report["yield_calls"], 1);
    assert_eq!(report["wait_calls"], 2);
    assert_eq!(report["timeouts"], 1);
    assert_eq!(report["gpu_retry_us"], 0);
}
