//! Mouse control through the wlr-virtual-pointer protocol, which places the
//! pointer at exact absolute positions on a chosen monitor (unlike uinput
//! tools, whose moves go through pointer acceleration). Blocking; run it off
//! the async runtime.

use anyhow::{Context, Result, bail};
use std::{thread, time::Duration, time::Instant};
use wayland_client::{
    Connection, Dispatch, QueueHandle,
    protocol::{
        wl_output::{self, WlOutput},
        wl_pointer::{Axis, AxisSource, ButtonState},
        wl_registry::{self, WlRegistry},
        wl_seat::WlSeat,
    },
};
use wayland_protocols_wlr::virtual_pointer::v1::client::{
    zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
    zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
};

/// Linux evdev button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;
/// Scroll distance of one wheel notch, matching libinput.
const NOTCH: f64 = 15.0;
const CLICK_GAP: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, Copy)]
pub enum Button {
    Left,
    Right,
    Middle,
}

#[derive(Debug, Clone, Copy)]
pub enum Action {
    /// Move to (x, y) within a monitor-sized area of `width` by `height`.
    Move {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
    },
    Press(Button),
    Release(Button),
    /// Wheel notches; positive scrolls down or right.
    Scroll {
        horizontal: bool,
        notches: i32,
    },
    Pause(Duration),
}

#[derive(Default)]
struct State {
    manager: Option<ZwlrVirtualPointerManagerV1>,
    seat: Option<WlSeat>,
    outputs: Vec<(WlOutput, String)>,
}

impl Dispatch<WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        queue: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        match interface.as_str() {
            "zwlr_virtual_pointer_manager_v1" if version >= 2 => {
                state.manager = Some(registry.bind(name, 2, queue, ()));
            }
            "wl_seat" if state.seat.is_none() => {
                state.seat = Some(registry.bind(name, 1, queue, ()));
            }
            "wl_output" if version >= 4 => {
                let output = registry.bind(name, 4, queue, ());
                state.outputs.push((output, String::new()));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = state.outputs.iter_mut().find(|(known, _)| known == output)
        {
            entry.1 = name;
        }
    }
}

macro_rules! ignore_events {
    ($($interface:ty),*) => {$(
        impl Dispatch<$interface, ()> for State {
            fn event(
                _: &mut Self,
                _: &$interface,
                _: <$interface as wayland_client::Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}
ignore_events!(WlSeat, ZwlrVirtualPointerManagerV1, ZwlrVirtualPointerV1);

/// Runs pointer actions on the named monitor.
pub fn run(monitor: &str, actions: &[Action]) -> Result<()> {
    let connection = Connection::connect_to_env().context("could not connect to the compositor")?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());
    let mut state = State::default();
    queue.roundtrip(&mut state)?;
    // Output names arrive after binding.
    queue.roundtrip(&mut state)?;

    let Some(manager) = &state.manager else {
        bail!("the compositor does not support wlr-virtual-pointer version 2");
    };
    let Some((output, _)) = state.outputs.iter().find(|(_, name)| name == monitor) else {
        bail!("no monitor named {monitor:?}");
    };
    let pointer =
        manager.create_virtual_pointer_with_output(state.seat.as_ref(), Some(output), &handle, ());
    let started = Instant::now();
    let time = || started.elapsed().as_millis() as u32;

    for action in actions {
        match *action {
            Action::Move {
                x,
                y,
                width,
                height,
            } => pointer.motion_absolute(time(), x, y, width, height),
            Action::Press(button) | Action::Release(button) => {
                let code = match button {
                    Button::Left => BTN_LEFT,
                    Button::Right => BTN_RIGHT,
                    Button::Middle => BTN_MIDDLE,
                };
                let state = if matches!(action, Action::Press(_)) {
                    ButtonState::Pressed
                } else {
                    ButtonState::Released
                };
                pointer.button(time(), code, state);
            }
            Action::Scroll {
                horizontal,
                notches,
            } => {
                let axis = if horizontal {
                    Axis::HorizontalScroll
                } else {
                    Axis::VerticalScroll
                };
                pointer.axis_source(AxisSource::Wheel);
                pointer.axis_discrete(time(), axis, f64::from(notches) * NOTCH, notches);
            }
            Action::Pause(duration) => {
                thread::sleep(duration);
                continue;
            }
        }
        pointer.frame();
        queue.roundtrip(&mut state)?;
        thread::sleep(Duration::from_millis(10));
    }
    pointer.destroy();
    queue.roundtrip(&mut state)?;
    Ok(())
}

/// Actions for clicking `count` times where the pointer is.
pub fn clicks(button: Button, count: u32) -> Vec<Action> {
    let mut actions = vec![Action::Pause(CLICK_GAP)];
    for _ in 0..count {
        actions.extend([
            Action::Press(button),
            Action::Release(button),
            Action::Pause(CLICK_GAP),
        ]);
    }
    actions
}
