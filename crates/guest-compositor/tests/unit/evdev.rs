use super::*;

#[test]
fn keyboard_and_mouse_buttons_keep_press_release_order_without_repeats() {
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    for (code, value) in [(28, 1), (28, 2), (28, 0), (0x110, 1), (0x110, 0)] {
        decoder.event(1, code, value, &mut events);
    }
    assert_eq!(
        events,
        vec![
            Event::Key {
                code: 28,
                down: true
            },
            Event::Key {
                code: 28,
                down: false
            },
            Event::Button {
                code: 0x110,
                down: true
            },
            Event::Button {
                code: 0x110,
                down: false
            }
        ]
    );
}

#[test]
fn tablet_coordinates_precede_the_click_and_are_normalized_from_device_ranges() {
    let mut decoder = Decoder {
        absolute: Some([
            Axis {
                value: 0,
                min: 0,
                max: 32767,
            },
            Axis {
                value: 0,
                min: 0,
                max: 32767,
            },
        ]),
        ..Default::default()
    };
    let mut events = Vec::new();
    decoder.event(3, 0, 32767, &mut events);
    decoder.event(3, 1, -1, &mut events);
    decoder.event(1, 0x110, 1, &mut events);
    decoder.event(0, 0, 0, &mut events);
    assert_eq!(
        events,
        vec![
            Event::Absolute { x: 1.0, y: 0.0 },
            Event::Button {
                code: 0x110,
                down: true
            }
        ]
    );
    assert_eq!(
        Axis {
            value: i32::MAX,
            min: i32::MIN,
            max: i32::MAX
        }
        .normalized(),
        1.0
    );
}

#[test]
fn dropped_packets_release_keys_and_ignore_the_incomplete_frame() {
    let mut decoder = Decoder::default();
    let mut events = Vec::new();
    decoder.event(1, 28, 1, &mut events);
    decoder.event(0, 3, 0, &mut events);
    decoder.event(1, 30, 1, &mut events);
    decoder.event(0, 0, 0, &mut events);
    decoder.event(1, 31, 1, &mut events);
    assert_eq!(
        events,
        vec![
            Event::Key {
                code: 28,
                down: true
            },
            Event::Reset,
            Event::Key {
                code: 31,
                down: true
            }
        ]
    );
}

#[test]
fn relative_axes_accumulate_and_precede_buttons() {
    let mut decoder = Decoder {
        relative_supported: true,
        ..Default::default()
    };
    let mut events = Vec::new();
    decoder.event(2, 0, 10, &mut events);
    decoder.event(2, 1, -7, &mut events);
    decoder.event(2, 0, 2, &mut events);
    decoder.event(1, 0x110, 1, &mut events);
    decoder.event(0, 0, 0, &mut events);
    assert_eq!(
        events,
        vec![
            Event::Relative {
                dx: 12.0,
                dy: -7.0,
                raw_dx: 12.0,
                raw_dy: -7.0
            },
            Event::Button {
                code: 0x110,
                down: true
            },
        ]
    );
}

#[test]
fn dropped_relative_packet_cannot_replay_stale_motion() {
    let mut decoder = Decoder {
        relative_supported: true,
        ..Default::default()
    };
    let mut events = Vec::new();
    decoder.event(2, 0, 100, &mut events);
    decoder.event(0, 3, 0, &mut events);
    decoder.event(2, 1, 200, &mut events);
    decoder.event(0, 0, 0, &mut events);
    decoder.event(2, 1, -3, &mut events);
    decoder.event(0, 0, 0, &mut events);
    assert_eq!(
        events,
        vec![
            Event::Reset,
            Event::Relative {
                dx: 0.0,
                dy: -3.0,
                raw_dx: 0.0,
                raw_dy: -3.0
            },
        ]
    );
}
