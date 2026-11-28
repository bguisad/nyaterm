use gpui::{InteractiveElement as _, MouseButton, StatefulInteractiveElement as _};

/// Seals a floating menu surface against the elements painted beneath it.
///
/// GPUI hitboxes do not implicitly occlude hitboxes behind them, so a menu
/// drawn over the path bar or the transfer list must own its pointer input:
/// otherwise a mouse-down that opens a menu item also reaches the underlying
/// `on_mouse_down` control (for example the breadcrumb children button), which
/// opens a second, unrelated menu.
pub(in crate::features::pages::transfers) fn menu_input_boundary(
    surface: gpui::Stateful<gpui::Div>,
) -> gpui::Stateful<gpui::Div> {
    surface
        .occlude()
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .on_scroll(|_, _, cx| cx.stop_propagation())
        .on_click(|_, _, cx| cx.stop_propagation())
}

#[cfg(test)]
mod tests;
