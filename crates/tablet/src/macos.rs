//! AppKit local event monitor for tablet data. The only module in the workspace that allows
//! `unsafe`: three calls into AppKit's Objective-C API that objc2 can't prove sound on its own
//! (each has a `SAFETY:` comment). Everything the monitor reads goes through the pure, tested
//! mapping in [`crate::appkit`].
//!
//! A *local* monitor sees this app's events only, on the main thread, inside `-[NSApplication
//! sendEvent:]` before the event reaches winit's view, so the sample is current when the UI
//! handles the matching pointer event. The monitor passes every event through unchanged.

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::Rc;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, MainThreadMarker};
use objc2_app_kit::{NSEvent, NSEventMask, NSPressureBehavior, NSPressureConfiguration, NSView};

use crate::appkit::{RawEvent, State, TrackpadUpdate};
use crate::{Error, Sample, Update, deliver, deliver_trackpad};

/// Keeps the monitor installed; dropping it removes the monitor. Main-thread only (not `Send`).
pub struct Monitor {
    token: Retained<AnyObject>,
    _handler: RcBlock<dyn Fn(NonNull<NSEvent>) -> *mut NSEvent>,
    configured_view: Rc<RefCell<Option<Retained<NSView>>>>,
}

impl Monitor {
    /// Install the monitor. `callback` gets `Some(sample)` for pen events and `None` when a mouse
    /// moves; it runs on the main thread. Call on the main thread (before or after the event loop
    /// starts).
    pub fn install(callback: impl Fn(Option<Sample>) + 'static) -> Result<Self, Error> {
        Self::install_with_trackpad(callback, |_| {}, || false)
    }

    /// Install pen and Force Touch readers on separate callbacks. The trackpad callback receives
    /// `None` at pressure end or mouse-up and never receives a pen sample.
    pub fn install_with_trackpad(
        callback: impl Fn(Option<Sample>) + 'static,
        trackpad_callback: impl Fn(Option<f32>) + 'static,
        enabled: impl Fn() -> bool + 'static,
    ) -> Result<Self, Error> {
        if MainThreadMarker::new().is_none() {
            return Err(Error::NotMainThread);
        }
        let state = RefCell::new(State::default());
        let configured_view: Rc<RefCell<Option<Retained<NSView>>>> = Rc::new(RefCell::new(None));
        let view_slot = configured_view.clone();
        let pressure_configuration = NSPressureConfiguration::initWithPressureBehavior(NSPressureConfiguration::alloc(), NSPressureBehavior::PrimaryGeneric);
        let handler = RcBlock::new(move |event: NonNull<NSEvent>| -> *mut NSEvent {
            // SAFETY: AppKit calls a local monitor's handler with the event it is about to
            // dispatch, a valid `NSEvent` that it keeps alive for the duration of the call; we
            // only borrow it within the call.
            let e: &NSEvent = unsafe { event.as_ref() };
            // Set the drawing-oriented pressure behavior on the app's view before the next
            // mouse-down whenever we first see its window. A newly created window gets its own
            // configuration. The first ever event may itself be mouse-down; in that case its
            // valid `pressure` field still provides the first dab when fractional.
            if matches!(e.r#type().0, crate::appkit::event_type::MOUSE_MOVED | crate::appkit::event_type::LEFT_MOUSE_DOWN)
                && let Some(mtm) = MainThreadMarker::new()
                && let Some(window) = e.window(mtm)
                && let Some(view) = window.contentView()
                && let Ok(mut slot) = view_slot.try_borrow_mut()
            {
                let same_view = slot.as_ref().is_some_and(|old| std::ptr::eq(&**old, &*view));
                if !enabled() {
                    if let Some(old) = slot.take() {
                        old.setPressureConfiguration(None);
                    }
                } else if !same_view {
                    if let Some(old) = slot.take() {
                        old.setPressureConfiguration(None);
                    }
                    view.setPressureConfiguration(Some(&pressure_configuration));
                    *slot = Some(view);
                }
            }
            let raw = read(e);
            // The handler can't re-enter itself (AppKit runs it synchronously on the main
            // thread), but `try_borrow_mut` keeps a re-entrant call from panicking anyway.
            if let Ok(mut st) = state.try_borrow_mut() {
                let (pen, trackpad) = st.handle_both(&raw);
                if let Update::Set(sample) = pen {
                    deliver(&callback, sample);
                }
                if let TrackpadUpdate::Set(pressure) = trackpad {
                    deliver_trackpad(&trackpad_callback, if enabled() { pressure } else { None });
                }
            }
            // Pass the event on unchanged.
            event.as_ptr()
        });
        let mask = NSEventMask(crate::appkit::event_mask());
        // SAFETY: the handler returns the (non-null, valid) event it was given, as the API
        // requires ("block's return must be a valid pointer or null"). We keep the block alive
        // in `Monitor` as well, although AppKit copies it.
        let token = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &handler) };
        let token = token.ok_or_else(|| Error::Platform("addLocalMonitorForEventsMatchingMask returned nil".into()))?;
        Ok(Self { token, _handler: handler, configured_view })
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        // SAFETY: `token` is exactly the object `addLocalMonitorForEventsMatchingMask:handler:`
        // returned, removed once (here). `Monitor` is not `Send`, so this runs on the main thread.
        unsafe { NSEvent::removeMonitor(&self.token) };
        if let Ok(mut slot) = self.configured_view.try_borrow_mut()
            && let Some(view) = slot.take()
        {
            view.setPressureConfiguration(None);
        }
    }
}

/// Read only the fields AppKit defines for this event type. In particular `subtype`, `pressure`,
/// and `stage` can raise Objective-C exceptions on unrelated event types.
fn read(e: &NSEvent) -> RawEvent {
    let kind = e.r#type().0;
    let tabletish = matches!(kind, crate::appkit::event_type::TABLET_POINT | crate::appkit::event_type::TABLET_PROXIMITY);
    let mouse = !tabletish && is_mouse(kind);
    // `subtype` raises an exception for event types without one; ask only mouse events.
    let subtype = if mouse { e.subtype().0 } else { 0 };
    let proximity = kind == crate::appkit::event_type::TABLET_PROXIMITY || (mouse && subtype == crate::appkit::subtype::TABLET_PROXIMITY);
    let point = kind == crate::appkit::event_type::TABLET_POINT || (mouse && subtype == crate::appkit::subtype::TABLET_POINT);
    let pressure_event = kind == crate::appkit::event_type::PRESSURE;
    let mouse_pressure = matches!(kind, crate::appkit::event_type::LEFT_MOUSE_DOWN | crate::appkit::event_type::LEFT_MOUSE_DRAGGED);
    let (pressure, tilt, rotation) = if point {
        let t = e.tilt();
        (e.pressure(), (t.x, t.y), e.rotation())
    } else if pressure_event || mouse_pressure {
        (e.pressure(), (0.0, 0.0), 0.0)
    } else {
        (0.0, (0.0, 0.0), 0.0)
    };
    let (device, entering) = if proximity { (e.pointingDeviceType().0, e.isEnteringProximity()) } else { (0, false) };
    let stage = if pressure_event { e.stage() } else { 0 };
    RawEvent { kind, subtype, pressure, stage, tilt, rotation, device, entering }
}

fn is_mouse(kind: usize) -> bool {
    use crate::appkit::event_type::*;
    matches!(
        kind,
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_graphics::{CGEvent, CGEventField, CGEventMouseSubtype, CGEventType, CGMouseButton};
    use objc2_foundation::NSPoint;

    #[test]
    fn install_off_the_main_thread_is_an_error() {
        // `cargo test` runs tests on worker threads, never on the main thread.
        let r = std::thread::spawn(|| Monitor::install(|_| {}).err()).join().unwrap();
        assert_eq!(r, Some(Error::NotMainThread));
    }

    /// A real `NSEvent` built from a Quartz event, as the tablet driver posts them.
    fn event(kind: CGEventType, set: impl Fn(&CGEvent)) -> objc2::rc::Retained<NSEvent> {
        // Quartz builds mouse events only; other types start blank and get their type set.
        let cg = CGEvent::new_mouse_event(None, kind, NSPoint::new(10.0, 10.0), CGMouseButton::Left)
            .or_else(|| {
                let e = CGEvent::new(None)?;
                CGEvent::set_type(Some(&e), kind);
                Some(e)
            })
            .expect("CGEvent");
        set(&cg);
        NSEvent::eventWithCGEvent(&cg).expect("NSEvent")
    }

    fn int(e: &CGEvent, f: CGEventField, v: i64) {
        CGEvent::set_integer_value_field(Some(e), f, v);
    }

    fn dbl(e: &CGEvent, f: CGEventField, v: f64) {
        CGEvent::set_double_value_field(Some(e), f, v);
    }

    /// Tablet mouse events read back through the same path as live ones.
    #[test]
    fn tablet_events_read_through_the_monitor_path() {
        let mut st = State::default();
        let drag = event(CGEventType::LeftMouseDragged, |e| {
            int(e, CGEventField::MouseEventSubtype, i64::from(CGEventMouseSubtype::TabletPoint.0));
            dbl(e, CGEventField::MouseEventPressure, 0.25);
            dbl(e, CGEventField::TabletEventPointPressure, 0.25);
            dbl(e, CGEventField::TabletEventTiltX, 0.5);
            dbl(e, CGEventField::TabletEventTiltY, -0.5);
            dbl(e, CGEventField::TabletEventRotation, 45.0);
        });
        let raw = read(&drag);
        assert_eq!((raw.kind, raw.subtype), (6, 1));
        let Update::Set(Some(s)) = st.handle(&raw) else { panic!("{raw:?}") };
        assert!((s.pressure - 0.25).abs() < 0.01, "{s:?}"); // Quartz keeps 8 bits of mouse pressure.
        assert!((s.tilt_x - 30.0).abs() < 0.1 && (s.tilt_y - 30.0).abs() < 0.1, "{s:?}");
        assert!((s.rotation - 45.0).abs() < 0.1, "{s:?}");
        assert!(!s.eraser);

        // Eraser end entering proximity, then a press with it.
        let prox = event(CGEventType::TabletProximity, |e| {
            int(e, CGEventField::TabletProximityEventPointerType, crate::appkit::device::ERASER as i64);
            int(e, CGEventField::TabletProximityEventEnterProximity, 1);
        });
        let raw = read(&prox);
        assert_eq!((raw.kind, raw.device, raw.entering), (24, 3, true));
        assert!(matches!(st.handle(&raw), Update::Set(Some(Sample { eraser: true, .. }))));

        // A plain mouse event (subtype 0) is a mouse, whatever its pressure.
        let mouse = event(CGEventType::LeftMouseDown, |e| dbl(e, CGEventField::MouseEventPressure, 0.7));
        let raw = read(&mouse);
        assert_eq!((raw.kind, raw.subtype), (1, 0));
        assert_eq!(st.handle(&raw), Update::Set(None));
    }

    #[test]
    fn fractional_mouse_pressure_reads_before_separate_pressure_events() {
        let mut state = State::default();
        let down = event(CGEventType::LeftMouseDown, |e| dbl(e, CGEventField::MouseEventPressure, 0.3));
        let raw = read(&down);
        assert_eq!((raw.kind, raw.subtype, raw.stage), (crate::appkit::event_type::LEFT_MOUSE_DOWN, 0, 0));
        assert!((raw.pressure - 0.3).abs() < 0.02, "Quartz rounds mouse pressure to 8 bits: {raw:?}");
        let (pen, trackpad) = state.handle_both(&raw);
        assert_eq!(pen, Update::Set(None));
        assert!(matches!(trackpad, TrackpadUpdate::Set(Some(p)) if (p - 0.3).abs() < 0.02));

        let up = event(CGEventType::LeftMouseUp, |_| {});
        assert_eq!(state.handle_both(&read(&up)), (Update::Set(None), TrackpadUpdate::Set(None)));
    }
}
