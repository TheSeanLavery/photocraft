//! AppKit `NSEvent` tablet fields → [`Sample`]s. Pure (no AppKit calls), so it is tested on
//! every platform; `macos.rs` reads the fields and feeds them here.
//!
//! - Mouse down/up/dragged/moved events whose `subtype` is `NSEventSubtypeTabletPoint` come from
//!   a pen: `pressure` (0..1), `tilt` (-1..1 per axis) and `rotation` (degrees) are valid.
//!   A mouse event with any other subtype is a mouse or trackpad: back to `None`. The pen's
//!   button-up reports pressure 0 and keeps the sample instead: it reaches the UI in the same
//!   frame as the stroke's last moves, which must keep the pressure they were drawn with.
//! - `NSEventTypeTabletPoint` events carry the same fields without a mouse event.
//! - `NSEventTypeTabletProximity` events (and mouse events with the proximity subtype) say a
//!   tool entered or left proximity and which one (`pointingDeviceType`: pen, eraser, cursor).

use crate::{Sample, Update};

/// `NSEventType` values (AppKit headers; stable since 10.0/10.4).
pub mod event_type {
    pub const LEFT_MOUSE_DOWN: usize = 1;
    pub const LEFT_MOUSE_UP: usize = 2;
    pub const RIGHT_MOUSE_DOWN: usize = 3;
    pub const RIGHT_MOUSE_UP: usize = 4;
    pub const MOUSE_MOVED: usize = 5;
    pub const LEFT_MOUSE_DRAGGED: usize = 6;
    pub const RIGHT_MOUSE_DRAGGED: usize = 7;
    pub const TABLET_POINT: usize = 23;
    pub const TABLET_PROXIMITY: usize = 24;
    pub const OTHER_MOUSE_DOWN: usize = 25;
    pub const OTHER_MOUSE_UP: usize = 26;
    pub const OTHER_MOUSE_DRAGGED: usize = 27;
    /// Force Touch pressure changes are a separate stream from mouse movement.
    pub const PRESSURE: usize = 34;
}

/// `NSEventSubtype` values of mouse events.
pub mod subtype {
    pub const MOUSE: i16 = 0;
    pub const TABLET_POINT: i16 = 1;
    pub const TABLET_PROXIMITY: i16 = 2;
    pub const TOUCH: i16 = 3;
}

/// `NSPointingDeviceType` values.
pub mod device {
    pub const UNKNOWN: usize = 0;
    pub const PEN: usize = 1;
    pub const CURSOR: usize = 2;
    pub const ERASER: usize = 3;
}

/// AppKit reports tilt as -1..1 of the device's range. Wacom pens report about ±60° at the
/// extremes (Qt maps AppKit tilt the same way).
pub const TILT_DEGREES: f64 = 60.0;

/// A visible light dab until AppKit delivers the first stage-1 pressure event. A mouse event's
/// 0/1 pressure is not a Force Touch reading, and treating 1 as such makes a full-size first dab.
const TRACKPAD_INITIAL_PRESSURE: f32 = 0.2;

/// The tablet-related fields of one `NSEvent`, read as plain values.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawEvent {
    /// `NSEventType`.
    pub kind: usize,
    /// `subtype` (only meaningful for mouse events).
    pub subtype: i16,
    pub pressure: f32,
    /// This mouse device can emit separate `PRESSURE` events (`associatedEventsMask`).
    pub pressure_capable: bool,
    /// Pressure gesture stage, meaningful only for `PRESSURE` events.
    pub stage: isize,
    /// `tilt` (x, y), each -1..1.
    pub tilt: (f64, f64),
    /// `rotation`, degrees.
    pub rotation: f32,
    /// `pointingDeviceType` (proximity events only).
    pub device: usize,
    /// `isEnteringProximity` (proximity events only).
    pub entering: bool,
}

/// The mask of event types [`State::handle`] maps (`NSEventMask` bits are `1 << type`).
pub fn event_mask() -> u64 {
    use event_type::*;
    [
        LEFT_MOUSE_DOWN,
        LEFT_MOUSE_UP,
        RIGHT_MOUSE_DOWN,
        RIGHT_MOUSE_UP,
        MOUSE_MOVED,
        LEFT_MOUSE_DRAGGED,
        RIGHT_MOUSE_DRAGGED,
        TABLET_POINT,
        TABLET_PROXIMITY,
        OTHER_MOUSE_DOWN,
        OTHER_MOUSE_UP,
        OTHER_MOUSE_DRAGGED,
        PRESSURE,
    ]
    .iter()
    .fold(0u64, |m, t| m | 1u64.checked_shl(*t as u32).unwrap_or(0))
}

/// Pen state across events: which end is in proximity.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    /// The tool in proximity is the eraser end.
    pub eraser: bool,
    left_down: bool,
    pen_active: bool,
}

/// A separate Force Touch reading. A mouse or pen event never becomes a pen sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TrackpadUpdate {
    Keep,
    Set(Option<f32>),
}

impl State {
    /// Map one event.
    pub fn handle(&mut self, e: &RawEvent) -> Update {
        self.handle_both(e).0
    }

    /// Map an event to independent pen and trackpad channels.
    pub fn handle_both(&mut self, e: &RawEvent) -> (Update, TrackpadUpdate) {
        use event_type::*;
        let mouse = matches!(
            e.kind,
            LEFT_MOUSE_DOWN
                | LEFT_MOUSE_UP
                | RIGHT_MOUSE_DOWN
                | RIGHT_MOUSE_UP
                | MOUSE_MOVED
                | LEFT_MOUSE_DRAGGED
                | RIGHT_MOUSE_DRAGGED
                | OTHER_MOUSE_DOWN
                | OTHER_MOUSE_UP
                | OTHER_MOUSE_DRAGGED
        );
        if e.kind == TABLET_PROXIMITY || (mouse && e.subtype == subtype::TABLET_PROXIMITY) {
            self.pen_active = e.entering;
            return (self.proximity(e), TrackpadUpdate::Set(None));
        }
        if e.kind == TABLET_POINT || (mouse && e.subtype == subtype::TABLET_POINT) {
            self.pen_active = true;
            if matches!(e.kind, LEFT_MOUSE_UP | RIGHT_MOUSE_UP | OTHER_MOUSE_UP) {
                // AppKit reports zero pressure for the release. Keep the last drawing sample
                // until the UI has consumed the stroke's remaining moves in this frame.
                return (Update::Keep, TrackpadUpdate::Set(None));
            }
            return (Update::Set(Some(self.sample(e))), TrackpadUpdate::Set(None));
        }
        if e.kind == LEFT_MOUSE_UP {
            self.left_down = false;
            // The UI may process this release in the same frame. Keep the last real pressure
            // until it has finished the stroke, then the UI clears the feed.
            return (Update::Set(None), TrackpadUpdate::Keep);
        }
        if e.kind == LEFT_MOUSE_DOWN {
            self.left_down = true;
            self.pen_active = false;
            // Apple exposes whether this click can have a separate pressure stream. An ordinary
            // mouse stays at full pressure; a Force Touch click starts light until its first
            // stage-1 reading arrives.
            let pressure = fractional_pressure(e.pressure).or_else(|| e.pressure_capable.then_some(TRACKPAD_INITIAL_PRESSURE));
            return (Update::Set(None), TrackpadUpdate::Set(pressure));
        }
        if e.kind == PRESSURE {
            if self.pen_active {
                return (Update::Keep, TrackpadUpdate::Keep);
            }
            // Stage 0 is the release transition, and stage 2 has its own pressure curve.
            // Neither is a stage-1 drawing sample; retain the last stage-1 value.
            if !self.left_down || e.stage != 1 {
                return (Update::Keep, TrackpadUpdate::Keep);
            }
            if let Some(pressure) = valid_pressure(e.pressure) {
                return (Update::Keep, TrackpadUpdate::Set(Some(pressure)));
            }
            return (Update::Keep, TrackpadUpdate::Keep);
        }
        if e.kind == LEFT_MOUSE_DRAGGED {
            let pressure = fractional_pressure(e.pressure);
            if self.left_down
                && let Some(pressure) = pressure
            {
                return (Update::Set(None), TrackpadUpdate::Set(Some(pressure)));
            }
            return (Update::Set(None), TrackpadUpdate::Keep);
        }
        if mouse { (Update::Set(None), TrackpadUpdate::Keep) } else { (Update::Keep, TrackpadUpdate::Keep) }
    }

    fn proximity(&mut self, e: &RawEvent) -> Update {
        if !e.entering {
            self.eraser = false;
            return Update::Set(None);
        }
        // A puck (cursor) is a mouse as far as painting goes.
        if e.device == device::CURSOR {
            self.eraser = false;
            return Update::Set(None);
        }
        self.eraser = e.device == device::ERASER;
        // Hovering: no pressure yet, but the end in use is known (the eraser switch happens now).
        Update::Set(Some(Sample { pressure: 0.0, tilt_x: 0.0, tilt_y: 0.0, rotation: 0.0, eraser: self.eraser }))
    }

    fn sample(&self, e: &RawEvent) -> Sample {
        let tilt = |v: f64| if v.is_finite() { (v.clamp(-1.0, 1.0) * TILT_DEGREES) as f32 } else { 0.0 };
        Sample {
            pressure: e.pressure,
            tilt_x: tilt(e.tilt.0),
            // AppKit's +y tilts away from the user; W3C's +y tilts towards the user.
            tilt_y: -tilt(e.tilt.1),
            rotation: e.rotation,
            eraser: self.eraser,
        }
        .sanitized()
    }
}

fn valid_pressure(pressure: f32) -> Option<f32> {
    pressure.is_finite().then(|| pressure.clamp(0.0, 1.0))
}

fn fractional_pressure(pressure: f32) -> Option<f32> {
    (pressure.is_finite() && pressure > 0.0 && pressure < 1.0).then_some(pressure)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pen(kind: usize, pressure: f32, tilt: (f64, f64), rotation: f32) -> RawEvent {
        RawEvent { kind, subtype: subtype::TABLET_POINT, pressure, tilt, rotation, ..Default::default() }
    }

    fn set(u: Update) -> Option<Sample> {
        match u {
            Update::Set(s) => s,
            Update::Keep => panic!("expected Set, got Keep"),
        }
    }

    #[test]
    fn tablet_mouse_events_carry_pressure_tilt_and_rotation() {
        let mut st = State::default();
        let s = set(st.handle(&pen(event_type::LEFT_MOUSE_DRAGGED, 0.5, (0.5, -0.25), 30.0))).unwrap();
        assert_eq!(s, Sample { pressure: 0.5, tilt_x: 30.0, tilt_y: 15.0, rotation: 30.0, eraser: false });
        let s = set(st.handle(&pen(event_type::TABLET_POINT, 0.8, (-1.0, 1.0), 0.0))).unwrap();
        assert_eq!((s.pressure, s.tilt_x, s.tilt_y), (0.8, -60.0, -60.0));
    }

    /// The UI paints a frame's moves at the sample current when it runs, after AppKit dispatched
    /// all of the frame's events: a lift's pressure 0 must not thin the stroke's last moves.
    #[test]
    fn lifting_the_pen_keeps_the_strokes_pressure() {
        let mut st = State::default();
        assert!(set(st.handle(&pen(event_type::LEFT_MOUSE_DRAGGED, 0.7, (0.0, 0.0), 0.0))).is_some());
        for kind in [event_type::LEFT_MOUSE_UP, event_type::RIGHT_MOUSE_UP, event_type::OTHER_MOUSE_UP] {
            assert_eq!(st.handle(&pen(kind, 0.0, (0.0, 0.0), 0.0)), Update::Keep, "type {kind}");
        }
        // Hovering afterwards reports the pen with no pressure.
        let s = set(st.handle(&pen(event_type::MOUSE_MOVED, 0.0, (0.0, 0.0), 0.0))).unwrap();
        assert_eq!(s.pressure, 0.0);
        // A mouse's button-up is still a mouse.
        let e = RawEvent { kind: event_type::LEFT_MOUSE_UP, subtype: subtype::MOUSE, ..Default::default() };
        assert_eq!(st.handle(&e), Update::Set(None));
    }

    #[test]
    fn mouse_events_are_full_pressure_mice() {
        let mut st = State::default();
        for sub in [subtype::MOUSE, subtype::TOUCH, 99] {
            let e = RawEvent { kind: event_type::LEFT_MOUSE_DOWN, subtype: sub, pressure: 1.0, ..Default::default() };
            assert_eq!(st.handle(&e), Update::Set(None), "subtype {sub}");
        }
        // Unrelated event types (keys and scroll) don't touch the pen sample.
        for kind in [0, 10, 22, 29, 34, usize::MAX] {
            assert_eq!(st.handle(&RawEvent { kind, ..Default::default() }), Update::Keep, "type {kind}");
        }
    }

    #[test]
    fn proximity_tracks_the_eraser_end() {
        let mut st = State::default();
        let prox = |device, entering| RawEvent { kind: event_type::TABLET_PROXIMITY, device, entering, ..Default::default() };
        let s = set(st.handle(&prox(device::ERASER, true))).unwrap();
        assert!(s.eraser && s.pressure == 0.0);
        assert!(set(st.handle(&pen(event_type::LEFT_MOUSE_DOWN, 0.4, (0.0, 0.0), 0.0))).unwrap().eraser);
        assert_eq!(st.handle(&prox(device::ERASER, false)), Update::Set(None));
        assert!(!st.eraser);
        assert!(!set(st.handle(&prox(device::PEN, true))).unwrap().eraser);
        assert!(!set(st.handle(&pen(event_type::LEFT_MOUSE_DRAGGED, 0.4, (0.0, 0.0), 0.0))).unwrap().eraser);
        // A puck is a mouse; unknown devices paint as a pen tip.
        assert_eq!(st.handle(&prox(device::CURSOR, true)), Update::Set(None));
        assert!(!set(st.handle(&prox(device::UNKNOWN, true))).unwrap().eraser);
        // Proximity as a mouse-event subtype works the same.
        let e = RawEvent { kind: event_type::MOUSE_MOVED, subtype: subtype::TABLET_PROXIMITY, device: device::ERASER, entering: true, ..Default::default() };
        assert!(set(st.handle(&e)).unwrap().eraser);
    }

    #[test]
    fn malformed_values_are_clamped_not_trusted() {
        let mut st = State::default();
        let s = set(st.handle(&pen(event_type::LEFT_MOUSE_DRAGGED, f32::NAN, (f64::NAN, f64::INFINITY), f32::NAN))).unwrap();
        assert_eq!(s, Sample { pressure: 1.0, tilt_x: 0.0, tilt_y: 0.0, rotation: 0.0, eraser: false });
        let s = set(st.handle(&pen(event_type::LEFT_MOUSE_DRAGGED, 4.0, (7.0, -9.0), -30.0))).unwrap();
        assert_eq!(s, Sample { pressure: 1.0, tilt_x: 60.0, tilt_y: 60.0, rotation: 330.0, eraser: false });
        let s = set(st.handle(&pen(event_type::LEFT_MOUSE_DRAGGED, -1.0, (f64::MAX, f64::MIN), f32::MAX))).unwrap();
        assert!(s.pressure == 0.0 && s.tilt_x == 60.0 && s.tilt_y == 60.0 && (0.0..360.0).contains(&s.rotation));
    }

    #[test]
    fn mask_covers_the_mapped_types() {
        let m = event_mask();
        for t in [1, 2, 3, 4, 5, 6, 7, 23, 24, 25, 26, 27, 34] {
            assert!(m & (1 << t) != 0, "{t}");
        }
        assert_eq!(m & (1 << 10), 0, "key down is not monitored");
    }

    #[test]
    fn trackpad_pressure_is_separate_from_pen_and_bounded_to_left_gesture() {
        let mut st = State::default();
        let pressure = |stage, pressure| RawEvent { kind: event_type::PRESSURE, stage, pressure, ..Default::default() };
        assert_eq!(st.handle_both(&pressure(1, 0.4)), (Update::Keep, TrackpadUpdate::Keep));
        let down = RawEvent { kind: event_type::LEFT_MOUSE_DOWN, pressure: 0.25, ..Default::default() };
        assert_eq!(st.handle_both(&down), (Update::Set(None), TrackpadUpdate::Set(Some(0.25))));
        assert_eq!(st.handle_both(&pressure(1, 0.6)), (Update::Keep, TrackpadUpdate::Set(Some(0.6))));
        let drag = RawEvent { kind: event_type::LEFT_MOUSE_DRAGGED, pressure: 1.0, ..Default::default() };
        assert_eq!(st.handle_both(&drag), (Update::Set(None), TrackpadUpdate::Keep));
        assert_eq!(st.handle_both(&pressure(2, 0.1)), (Update::Keep, TrackpadUpdate::Keep));
        assert_eq!(st.handle_both(&pressure(0, 0.0)), (Update::Keep, TrackpadUpdate::Keep));
        assert_eq!(st.handle_both(&RawEvent { kind: event_type::LEFT_MOUSE_UP, ..Default::default() }), (Update::Set(None), TrackpadUpdate::Keep));
        assert_eq!(st.handle_both(&pressure(1, 0.7)), (Update::Keep, TrackpadUpdate::Keep));
    }

    #[test]
    fn trackpad_edges_never_invent_full_pressure() {
        let mut st = State::default();
        let down = RawEvent { kind: event_type::LEFT_MOUSE_DOWN, pressure: 1.0, pressure_capable: true, ..Default::default() };
        assert_eq!(st.handle_both(&down).1, TrackpadUpdate::Set(Some(TRACKPAD_INITIAL_PRESSURE)));
        let pressure = |stage, pressure| RawEvent { kind: event_type::PRESSURE, stage, pressure, ..Default::default() };
        assert_eq!(st.handle_both(&pressure(1, 0.35)).1, TrackpadUpdate::Set(Some(0.35)));
        assert_eq!(st.handle_both(&pressure(2, 0.0)).1, TrackpadUpdate::Keep, "stage 2 uses a different curve");
        assert_eq!(st.handle_both(&pressure(0, 0.0)).1, TrackpadUpdate::Keep, "release preserves the final drawing sample");
        assert_eq!(st.handle_both(&RawEvent { kind: event_type::LEFT_MOUSE_UP, ..Default::default() }).1, TrackpadUpdate::Keep);
        assert_eq!(st.handle_both(&down).1, TrackpadUpdate::Set(Some(TRACKPAD_INITIAL_PRESSURE)), "the next stroke starts light again");
        let mouse = RawEvent { kind: event_type::LEFT_MOUSE_DOWN, pressure: 1.0, ..Default::default() };
        assert_eq!(st.handle_both(&mouse).1, TrackpadUpdate::Set(None), "a regular mouse keeps its full-pressure fallback");
    }

    #[test]
    fn fractional_mouse_drag_seeds_trackpad_pressure_but_pen_stays_separate() {
        let mut st = State::default();
        let down = RawEvent { kind: event_type::LEFT_MOUSE_DOWN, pressure: 1.0, ..Default::default() };
        assert_eq!(st.handle_both(&down).1, TrackpadUpdate::Set(None));
        let drag = RawEvent { kind: event_type::LEFT_MOUSE_DRAGGED, pressure: 0.5, ..Default::default() };
        assert_eq!(st.handle_both(&drag).1, TrackpadUpdate::Set(Some(0.5)));
        let pen = RawEvent { kind: event_type::TABLET_POINT, pressure: 0.8, ..Default::default() };
        let (sample, trackpad) = st.handle_both(&pen);
        assert!(matches!(sample, Update::Set(Some(Sample { pressure: 0.8, .. }))));
        assert_eq!(trackpad, TrackpadUpdate::Set(None));
        let invalid = RawEvent { kind: event_type::PRESSURE, stage: 1, pressure: f32::NAN, ..Default::default() };
        assert_eq!(st.handle_both(&invalid).1, TrackpadUpdate::Keep);
    }
}
