//! Settings › Interface size: zooms the web view and scales the window with it,
//! like the client's window sizes, so the layout keeps the room it was designed for.

use tauri::{LogicalSize, Manager, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewWindow};

pub const MIN_UI_SCALE: u32 = 80;
pub const MAX_UI_SCALE: u32 = 200;

/// Zoom `window` to `to` percent. Its size and minimum size follow, from the
/// `from` percent it was shown at, but never outgrow the screen's work area.
/// A maximized window keeps filling the screen.
pub fn apply(window: &WebviewWindow, from: u32, to: u32) {
    let zoom = f64::from(to) / 100.0;
    let _ = window.set_zoom(zoom);
    let Ok(Some(monitor)) = window.current_monitor() else { return };
    let dpi = monitor.scale_factor();
    let area = *monitor.work_area();
    if let Some(config) = window.config().app.windows.iter().find(|config| config.label == window.label()) {
        if let (Some(width), Some(height)) = (config.min_width, config.min_height) {
            let screen = area.size.to_logical::<f64>(dpi);
            let min = LogicalSize::new((width * zoom).min(screen.width), (height * zoom).min(screen.height));
            let _ = window.set_min_size(Some(min));
        }
    }
    if from == to || window.is_maximized().unwrap_or(true) || window.is_fullscreen().unwrap_or(true) {
        return;
    }
    let (Ok(position), Ok(size)) = (window.outer_position(), window.outer_size()) else { return };
    let (position, size) = scaled(position, size, f64::from(to) / f64::from(from), area);
    let _ = window.set_size(size);
    let _ = window.set_position(position);
}

/// The window scaled by `ratio`: at most the size of the work area, centered
/// where it was, and moved inside the work area when it would overflow it.
fn scaled(
    position: PhysicalPosition<i32>,
    size: PhysicalSize<u32>,
    ratio: f64,
    area: PhysicalRect<i32, u32>,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let fit = |length: u32, limit: u32| ((f64::from(length) * ratio).round() as u32).min(limit);
    let target = PhysicalSize::new(fit(size.width, area.size.width), fit(size.height, area.size.height));
    let place = |start: i32, length: u32, new: u32, area_start: i32, area_length: u32| {
        let center = start + (length / 2) as i32;
        (center - (new / 2) as i32)
            .min(area_start + (area_length - new) as i32)
            .max(area_start)
    };
    let x = place(position.x, size.width, target.width, area.position.x, area.size.width);
    let y = place(position.y, size.height, target.height, area.position.y, area.size.height);
    (PhysicalPosition::new(x, y), target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(x: i32, y: i32, width: u32, height: u32) -> PhysicalRect<i32, u32> {
        PhysicalRect {
            position: PhysicalPosition::new(x, y),
            size: PhysicalSize::new(width, height),
        }
    }

    fn run(position: (i32, i32), size: (u32, u32), ratio: f64, work: PhysicalRect<i32, u32>) -> ((i32, i32), (u32, u32)) {
        let (position, size) = scaled(position.into(), size.into(), ratio, work);
        ((position.x, position.y), (size.width, size.height))
    }

    #[test]
    fn grows_around_its_center() {
        // 1440p, centered default window, 125%.
        let work = area(0, 0, 2560, 1392);
        assert_eq!(run((600, 266), (1360, 860), 1.25, work), ((430, 159), (1700, 1075)));
        // Back to 100% returns to the same place.
        assert_eq!(run((430, 159), (1700, 1075), 0.8, work), ((600, 266), (1360, 860)));
    }

    #[test]
    fn stays_inside_the_work_area() {
        let work = area(0, 0, 2560, 1392);
        // Near the top-left corner: pushed back on screen.
        assert_eq!(run((0, 0), (1360, 860), 1.5, work), ((0, 0), (2040, 1290)));
        // Larger than the screen: capped to it.
        assert_eq!(run((600, 266), (1360, 860), 2.0, work), ((0, 0), (2560, 1392)));
        // A second monitor left of the primary one, taskbar on top.
        let left = area(-1920, 40, 1920, 1040);
        assert_eq!(run((-1800, 900), (1360, 860), 1.1, left), ((-1868, 134), (1496, 946)));
    }
}
