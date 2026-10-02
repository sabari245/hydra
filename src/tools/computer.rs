//! Screen, mouse, and keyboard control.
//!
//! The action set, parameters, coordinate handling, and messages are ported
//! from the `computer` tool in Anthropic's computer-use reference
//! implementation (anthropics/anthropic-quickstarts,
//! computer-use-best-practices/computer_use/tools/computer.py, MIT). The
//! model works in the pixel space of the most recent screenshot and the tool
//! scales coordinates back to the monitor. Instead of pyautogui on macOS,
//! this uses libwayshot for screenshots, the wlr-virtual-pointer protocol for
//! the mouse, and wtype for the keyboard. Actions the Wayland backends cannot
//! do (holding keys, modifier clicks, reading the cursor position) are left
//! out, and screenshots can target any monitor.

use super::ToolResult;
use crate::{
    pointer::{self, Action, Button},
    screenshot::{self, Monitor},
    typing,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
    time::Duration,
};

const KEY_REPEAT_MAX: u64 = 100;

const ACTIONS: &[&str] = &[
    "screenshot",
    "left_click",
    "double_click",
    "triple_click",
    "right_click",
    "middle_click",
    "mouse_move",
    "left_click_drag",
    "scroll",
    "type",
    "key",
    "read_clipboard",
    "write_clipboard",
    "wait",
    "zoom",
];

pub fn definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "computer",
            "description": "Control the user's Linux desktop: take screenshots of any monitor, move and click the mouse, type, press keys, scroll, and read or write the clipboard. Coordinates are pixels in the most recent screenshot, origin top-left.",
            "parameters": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ACTIONS,
                        "description": "* screenshot: capture `monitor` (the focused monitor if omitted). Later coordinates refer to this screenshot.\n* left_click / double_click / triple_click / right_click / middle_click: click at `coordinate` (or where the pointer already is if omitted).\n* mouse_move: move the pointer to `coordinate`.\n* left_click_drag: drag from `start_coordinate` to `coordinate`.\n* scroll: scroll at `coordinate` (or the pointer position) in `scroll_direction` by `scroll_amount` notches.\n* type: type literal `text` at the current focus; a line break presses Enter and a tab presses Tab.\n* key: press a chord like 'ctrl+shift+t' or a single key such as 'Return', 'Tab', 'Escape', 'BackSpace', 'Page_Down', 'F5', `repeat` times (default once).\n* read_clipboard / write_clipboard: get or set clipboard `text`.\n* wait: sleep `duration` seconds.\n* zoom: return a cropped, higher-detail view of the screen region `region` = [x1, y1, x2, y2]. Use this to read small text or inspect fine detail. Coordinates in later actions still refer to the full screenshot, not the zoom."
                    },
                    "monitor": { "type": "string", "description": "Monitor name for screenshot, for example HDMI-A-1." },
                    "coordinate": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
                    "start_coordinate": { "type": "array", "items": { "type": "integer" }, "minItems": 2, "maxItems": 2 },
                    "text": { "type": "string" },
                    "repeat": { "type": "integer", "minimum": 1, "maximum": KEY_REPEAT_MAX },
                    "scroll_direction": { "type": "string", "enum": ["up", "down", "left", "right"] },
                    "scroll_amount": { "type": "integer", "minimum": 1 },
                    "duration": { "type": "number", "minimum": 0, "maximum": 60 },
                    "region": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "minItems": 4,
                        "maxItems": 4,
                        "description": "[x1, y1, x2, y2] in the same image space as `coordinate`."
                    }
                },
                "required": ["action"]
            }
        }
    })
}

/// The most recent screenshot, which defines the coordinate space.
#[derive(Clone)]
struct View {
    monitor: Monitor,
    width: u32,
    height: u32,
}

pub struct Computer {
    view: Option<View>,
    max_size: u32,
}

/// Key names models tend to emit, mapped to XKB keysym names for wtype.
/// Anything not listed passes through unchanged.
fn translate_key(key: &str) -> String {
    let key = key.trim();
    let mapped = match key.to_lowercase().as_str() {
        "return" | "enter" => "Return",
        "esc" | "escape" => "Escape",
        "tab" => "Tab",
        "backspace" => "BackSpace",
        "delete" | "del" => "Delete",
        "space" => "space",
        "page_up" | "pageup" | "prior" => "Page_Up",
        "page_down" | "pagedown" | "next" => "Page_Down",
        "home" => "Home",
        "end" => "End",
        "insert" => "Insert",
        "up" | "arrowup" => "Up",
        "down" | "arrowdown" => "Down",
        "left" | "arrowleft" => "Left",
        "right" | "arrowright" => "Right",
        "printscreen" | "print" => "Print",
        lower if lower.len() > 1 && lower.starts_with('f') && lower[1..].parse::<u8>().is_ok() => {
            return key.to_uppercase();
        }
        _ => return key.to_owned(),
    };
    mapped.to_owned()
}

fn modifier(key: &str) -> Option<&'static str> {
    Some(match key.trim().to_lowercase().as_str() {
        "ctrl" | "control" => "ctrl",
        "shift" => "shift",
        "alt" | "option" => "alt",
        "super" | "meta" | "cmd" | "command" | "win" | "windows" | "logo" => "logo",
        _ => return None,
    })
}

/// Presses a chord such as "ctrl+shift+t" with wtype.
fn press_chord(chord: &str) -> Result<String> {
    let parts: Vec<&str> = chord.split('+').filter(|part| !part.is_empty()).collect();
    let Some((key, modifiers)) = parts.split_last() else {
        bail!("key requires `text`, such as 'ctrl+l' or 'Return'");
    };
    let modifiers = modifiers
        .iter()
        .map(|name| modifier(name).with_context(|| format!("unknown modifier key {name:?}")))
        .collect::<Result<Vec<_>>>()?;
    let key = translate_key(key);
    let mut args = Vec::new();
    for name in &modifiers {
        args.extend(["-M", name]);
    }
    args.extend(["-k", &key]);
    for name in modifiers.iter().rev() {
        args.extend(["-m", name]);
    }
    typing::wtype(&args, chord)?;
    let mut pressed = modifiers.join("+");
    if !pressed.is_empty() {
        pressed.push('+');
    }
    pressed.push_str(&key);
    Ok(pressed)
}

fn coordinate(arguments: &Value, name: &str) -> Result<Option<[i64; 2]>> {
    let Some(value) = arguments.get(name).filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    match value.as_array().map(Vec::as_slice) {
        Some([x, y]) => match (x.as_i64(), y.as_i64()) {
            (Some(x), Some(y)) => Ok(Some([x, y])),
            _ => bail!("`{name}` must be two integers"),
        },
        _ => bail!("`{name}` must be [x, y]"),
    }
}

fn clipboard_read() -> Result<String> {
    let output = Command::new("wl-paste")
        .arg("--no-newline")
        .output()
        .context("could not run wl-paste; install wl-clipboard")?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn clipboard_write(text: &str) -> Result<()> {
    let mut child = Command::new("wl-copy")
        .stdin(Stdio::piped())
        .spawn()
        .context("could not run wl-copy; install wl-clipboard")?;
    child
        .stdin
        .take()
        .context("wl-copy has no stdin")?
        .write_all(text.as_bytes())?;
    child.wait()?;
    Ok(())
}

impl Computer {
    pub fn new(max_size: u32) -> Self {
        Self {
            view: None,
            max_size,
        }
    }

    fn view(&self) -> Result<View> {
        self.view
            .clone()
            .context("take a screenshot first; coordinates refer to the most recent screenshot")
    }

    /// Image pixels to monitor-local logical pixels, clamped to the monitor.
    fn to_monitor(view: &View, [x, y]: [i64; 2]) -> (u32, u32) {
        let scale = |value: i64, image: u32, monitor: u32| -> u32 {
            let scaled = (value as f64 * f64::from(monitor) / f64::from(image)).round();
            scaled.clamp(0.0, f64::from(monitor.saturating_sub(1))) as u32
        };
        (
            scale(x, view.width, view.monitor.width),
            scale(y, view.height, view.monitor.height),
        )
    }

    fn at(view: &View, [x, y]: [i64; 2]) -> String {
        format!("({x}, {y}) in {}x{} image", view.width, view.height)
    }

    fn move_to(view: &View, point: [i64; 2]) -> Action {
        let (x, y) = Self::to_monitor(view, point);
        Action::Move {
            x,
            y,
            width: view.monitor.width,
            height: view.monitor.height,
        }
    }

    async fn pointer(view: &View, actions: Vec<Action>) -> Result<()> {
        let monitor = view.monitor.name.clone();
        tokio::task::spawn_blocking(move || pointer::run(&monitor, &actions)).await?
    }

    pub async fn execute(&mut self, arguments: &Value) -> Result<ToolResult> {
        let action = super::string(arguments, "action")?;
        let text = || super::string(arguments, "text");
        let point = coordinate(arguments, "coordinate")?;

        match action {
            "screenshot" => {
                let monitor = arguments
                    .get("monitor")
                    .and_then(Value::as_str)
                    .filter(|monitor| !monitor.is_empty())
                    .map(str::to_owned);
                let max_size = self.max_size;
                let shot = tokio::task::spawn_blocking(move || {
                    screenshot::capture(monitor.as_deref(), max_size)
                })
                .await??;
                self.view = Some(View {
                    monitor: shot.monitor.clone(),
                    width: shot.width,
                    height: shot.height,
                });
                Ok(ToolResult::Image(shot.describe(), shot))
            }
            "left_click" | "double_click" | "triple_click" | "right_click" | "middle_click" => {
                let view = self.view()?;
                let button = match action {
                    "right_click" => Button::Right,
                    "middle_click" => Button::Middle,
                    _ => Button::Left,
                };
                let count = match action {
                    "double_click" => 2,
                    "triple_click" => 3,
                    _ => 1,
                };
                let mut actions = Vec::new();
                if let Some(point) = point {
                    actions.push(Self::move_to(&view, point));
                }
                actions.extend(pointer::clicks(button, count));
                Self::pointer(&view, actions).await?;
                let place = point.map_or_else(
                    || " at the pointer".to_owned(),
                    |point| format!(" at {}", Self::at(&view, point)),
                );
                Ok(ToolResult::Text(format!("{action}{place}")))
            }
            "mouse_move" => {
                let view = self.view()?;
                let point = point.context("mouse_move requires `coordinate`")?;
                Self::pointer(&view, vec![Self::move_to(&view, point)]).await?;
                Ok(ToolResult::Text(format!(
                    "moved to {}",
                    Self::at(&view, point)
                )))
            }
            "left_click_drag" => {
                let view = self.view()?;
                let (Some(start), Some(end)) = (coordinate(arguments, "start_coordinate")?, point)
                else {
                    bail!("left_click_drag requires `start_coordinate` and `coordinate`");
                };
                let pause = Action::Pause(Duration::from_millis(100));
                let actions = vec![
                    Self::move_to(&view, start),
                    pause,
                    Action::Press(Button::Left),
                    pause,
                    Self::move_to(&view, end),
                    pause,
                    Action::Release(Button::Left),
                ];
                Self::pointer(&view, actions).await?;
                Ok(ToolResult::Text(format!(
                    "dragged {} -> {}",
                    Self::at(&view, start),
                    Self::at(&view, end)
                )))
            }
            "scroll" => {
                let view = self.view()?;
                let direction = super::string(arguments, "scroll_direction")?;
                let amount = arguments
                    .get("scroll_amount")
                    .and_then(Value::as_i64)
                    .unwrap_or(3)
                    .clamp(1, 100) as i32;
                let (horizontal, notches) = match direction {
                    "up" => (false, -amount),
                    "down" => (false, amount),
                    "left" => (true, -amount),
                    "right" => (true, amount),
                    _ => bail!("scroll_direction must be up, down, left, or right"),
                };
                let mut actions = Vec::new();
                if let Some(point) = point {
                    actions.push(Self::move_to(&view, point));
                }
                actions.push(Action::Scroll {
                    horizontal,
                    notches,
                });
                Self::pointer(&view, actions).await?;
                let place = point.map_or_else(
                    || " at the pointer".to_owned(),
                    |point| format!(" at {}", Self::at(&view, point)),
                );
                Ok(ToolResult::Text(format!(
                    "scrolled {direction} by {amount}{place}"
                )))
            }
            "type" => {
                let text = text()?.to_owned();
                let characters = text.chars().count();
                tokio::task::spawn_blocking(move || typing::type_keys(&text)).await??;
                Ok(ToolResult::Text(format!("typed {characters} chars")))
            }
            "key" => {
                let chord = text()?.to_owned();
                let repeat = arguments.get("repeat").and_then(Value::as_u64).unwrap_or(1);
                if !(1..=KEY_REPEAT_MAX).contains(&repeat) {
                    bail!("`repeat` must be an integer in 1..{KEY_REPEAT_MAX}");
                }
                let pressed = tokio::task::spawn_blocking(move || {
                    let mut pressed = String::new();
                    for _ in 0..repeat {
                        pressed = press_chord(&chord)?;
                    }
                    anyhow::Ok(pressed)
                })
                .await??;
                let times = if repeat > 1 {
                    format!(" x{repeat}")
                } else {
                    String::new()
                };
                Ok(ToolResult::Text(format!("pressed {pressed}{times}")))
            }
            "read_clipboard" => Ok(ToolResult::Text(clipboard_read()?)),
            "write_clipboard" => {
                clipboard_write(text()?)?;
                Ok(ToolResult::Text("clipboard set".to_owned()))
            }
            "wait" => {
                let seconds = arguments
                    .get("duration")
                    .and_then(Value::as_f64)
                    .unwrap_or(1.0)
                    .clamp(0.0, 60.0);
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                Ok(ToolResult::Text(format!("waited {seconds}s")))
            }
            "zoom" => self.zoom(arguments).await,
            other => bail!("unknown action: {other}"),
        }
    }

    async fn zoom(&self, arguments: &Value) -> Result<ToolResult> {
        let view = self.view()?;
        let region: Vec<i64> = arguments
            .get("region")
            .and_then(Value::as_array)
            .map(|values| values.iter().filter_map(Value::as_i64).collect())
            .unwrap_or_default();
        let [x1, y1, x2, y2] = region[..] else {
            bail!("zoom requires `region` = [x1, y1, x2, y2]");
        };
        if x2 <= x1 || y2 <= y1 {
            bail!("zoom region must have x2 > x1 and y2 > y1");
        }
        let max_size = self.max_size;
        let monitor = view.monitor.name.clone();
        let (width, height) = (view.width, view.height);
        let shot = tokio::task::spawn_blocking(move || {
            // Capture at full resolution so the crop carries more detail than
            // the same area did in the downscaled screenshot.
            let (full, monitor) = screenshot::capture_raw(Some(&monitor))?;
            let fx = f64::from(full.width()) / f64::from(width);
            let fy = f64::from(full.height()) / f64::from(height);
            let left = ((x1 as f64 * fx).round().max(0.0) as u32).min(full.width() - 1);
            let top = ((y1 as f64 * fy).round().max(0.0) as u32).min(full.height() - 1);
            let right = ((x2 as f64 * fx).round() as u32).clamp(left + 1, full.width());
            let bottom = ((y2 as f64 * fy).round() as u32).clamp(top + 1, full.height());
            let crop = full.crop_imm(left, top, right - left, bottom - top);
            screenshot::encode(&crop, monitor, max_size)
        })
        .await??;
        let description = format!(
            "zoom of ({x1},{y1})-({x2},{y2}) in {width}x{height} image, shown at {}x{}. \
             Subsequent coordinates still refer to the full {width}x{height} screenshot, not \
             this crop.",
            shot.width, shot.height
        );
        Ok(ToolResult::Image(description, shot))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_map_to_xkb_names() {
        assert_eq!(translate_key("enter"), "Return");
        assert_eq!(translate_key("PageDown"), "Page_Down");
        assert_eq!(translate_key("f5"), "F5");
        assert_eq!(translate_key("l"), "l");
        assert_eq!(modifier("Control"), Some("ctrl"));
        assert_eq!(modifier("cmd"), Some("logo"));
        assert_eq!(modifier("x"), None);
    }

    #[test]
    fn coordinates_scale_to_the_monitor() {
        let view = View {
            monitor: Monitor {
                name: "HDMI-A-1".to_owned(),
                description: String::new(),
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            width: 1280,
            height: 720,
        };
        assert_eq!(Computer::to_monitor(&view, [640, 360]), (960, 540));
        assert_eq!(Computer::to_monitor(&view, [5000, -3]), (1919, 0));
    }
}
