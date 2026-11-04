//! Wayland touchpad pinch gestures.

use std::ops::Deref;
use std::sync::Mutex;

use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::{
    Event as PinchEvent, ZwpPointerGesturePinchV1,
};
use wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;

use crate::event::{DeviceId, TouchPhase, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, WindowId};

pub struct PointerGesturesState {
    manager: ZwpPointerGesturesV1,
}

impl PointerGesturesState {
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // v1 defines pinch; v3 adds the optional hold gesture.
        let manager = globals.bind(queue_handle, 1..=3, GlobalData)?;
        Ok(Self { manager })
    }
}

impl Deref for PointerGesturesState {
    type Target = ZwpPointerGesturesV1;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

/// Convert the compositor's cumulative scale into the additive delta consumed by
/// winit clients. The caller updates `previous` only for a valid scale.
fn pinch_delta(previous: f64, scale: f64) -> Option<f64> {
    if !scale.is_finite() || scale <= 0.0 {
        return None;
    }
    let delta = (scale / previous).ln();
    delta.is_finite().then_some(delta)
}

#[derive(Default)]
pub struct PinchData {
    state: Mutex<Option<(WindowId, f64)>>,
}

impl Dispatch<ZwpPointerGesturesV1, GlobalData, WinitState> for PointerGesturesState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpPointerGesturesV1,
        _event: <ZwpPointerGesturesV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        // The manager has no events.
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, PinchData, WinitState> for PointerGesturesState {
    fn event(
        state: &mut WinitState,
        _proxy: &ZwpPointerGesturePinchV1,
        event: <ZwpPointerGesturePinchV1 as Proxy>::Event,
        data: &PinchData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let mut gesture = data.state.lock().unwrap();
        let (window_id, delta, phase) = match event {
            PinchEvent::Begin { surface, fingers, .. } => {
                // Ignore decorations and unknown windows. The protocol sends a cumulative
                // scale relative to the beginning of each gesture.
                let window_id = wayland::make_wid(&surface);
                if fingers != 2 || !state.windows.get_mut().contains_key(&window_id) {
                    *gesture = None;
                    return;
                }
                *gesture = Some((window_id, 1.0));
                (window_id, 0.0, TouchPhase::Started)
            },
            PinchEvent::Update { scale, .. } => {
                let Some((window_id, previous)) = gesture.as_mut() else { return };
                // Sketch applies exp(delta). Successive compositor scales must
                // yield the change since the previous update, not cumulative zoom.
                let Some(delta) = pinch_delta(*previous, scale) else { return };
                *previous = scale;
                (*window_id, delta, TouchPhase::Moved)
            },
            PinchEvent::End { cancelled, .. } => {
                let Some((window_id, _)) = gesture.take() else { return };
                let phase = if cancelled == 0 { TouchPhase::Ended } else { TouchPhase::Cancelled };
                (window_id, 0.0, phase)
            },
            _ => return,
        };
        if !delta.is_finite() || !state.windows.get_mut().contains_key(&window_id) {
            return;
        }
        let device_id = DeviceId(crate::platform_impl::DeviceId::Wayland(wayland::DeviceId));
        state
            .events_sink
            .push_window_event(WindowEvent::PinchGesture { device_id, delta, phase }, window_id);
    }
}

delegate_dispatch!(WinitState: [ZwpPointerGesturesV1: GlobalData] => PointerGesturesState);
delegate_dispatch!(WinitState: [ZwpPointerGesturePinchV1: PinchData] => PointerGesturesState);

#[cfg(test)]
mod tests {
    use super::pinch_delta;

    #[test]
    fn cumulative_scales_produce_incremental_zoom() {
        let first = pinch_delta(1.0, 1.25).unwrap();
        let second = pinch_delta(1.25, 1.5).unwrap();
        assert!(((first + second).exp() - 1.5).abs() < 1e-12);
        assert!((pinch_delta(1.5, 1.0).unwrap().exp() - 1.0 / 1.5).abs() < 1e-12);
    }

    #[test]
    fn invalid_scales_are_ignored() {
        for scale in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(pinch_delta(1.0, scale), None);
        }
    }
}
