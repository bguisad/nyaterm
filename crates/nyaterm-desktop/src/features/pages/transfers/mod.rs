mod path_completion;
use gpui::{
    Context, IntoElement, MouseButton, StatefulInteractiveElement as _, div, prelude::*, px,
};

use self::browser::transfer_browser_view;
use self::panel::TransferPanel;
use self::queue::transfer_queue_view;
use self::resize::transfer_height_resize_handle;

mod browser;
mod browser_columns;
mod browser_filter;
mod browser_keys;
mod browser_navigation;
mod browser_selection;
mod context_menu_policy;
mod drag_preview;
mod duplicate_banner;
mod editor;
mod entry_row;
pub(in crate::features) mod file_ops;
mod flush;
mod forms;
mod helpers;
mod overlays;
mod overlays_context;
mod overlays_delete_move;
mod overlays_editor;
mod overlays_favorites;
mod overlays_unknown;
mod overlays_upload;
pub(in crate::features) mod panel;
pub(in crate::features::pages::transfers) mod path_bar;
mod preview;
mod privilege;
mod properties;
mod properties_dialog;
mod queue;
mod resize;
mod terminal_directory;
#[cfg(test)]
pub(in crate::features::pages::transfers) mod tests_support;
mod tree;

pub(in crate::features) use entry_row::format_browser_file_size;
use entry_row::{
    TransferBrowserEntryRowPresentation, transfer_browser_entry_row,
    transfer_browser_parent_entry_row,
};
pub(in crate::features) use helpers::format_sftp_modified;
use helpers::*;

const NATIVE_EDITOR_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::features) enum TransferBrowserAvailability {
    NoSession,
    UnsupportedSession,
    DisconnectedSession,
    Browsable,
}

fn transfer_browser_availability(
    has_active_session: bool,
    has_browser_backend: bool,
    is_disconnected: bool,
) -> TransferBrowserAvailability {
    if !has_active_session {
        TransferBrowserAvailability::NoSession
    } else if !has_browser_backend {
        TransferBrowserAvailability::UnsupportedSession
    } else if is_disconnected {
        TransferBrowserAvailability::DisconnectedSession
    } else {
        TransferBrowserAvailability::Browsable
    }
}

fn transfer_dialog_width(viewport_width: f32, preferred_width: f32) -> f32 {
    preferred_width.min((viewport_width - 32.).max(240.))
}

fn transfer_menu_position(
    x: f32,
    y: f32,
    menu_width: f32,
    preferred_height: f32,
    viewport_width: f32,
    viewport_height: f32,
) -> (f32, f32, f32) {
    let margin = 8.;
    let max_height = (viewport_height - margin * 2.).max(80.);
    let height = preferred_height.min(max_height);
    let max_x = (viewport_width - menu_width - margin).max(margin);
    let max_y = (viewport_height - height - margin).max(margin);
    (x.clamp(margin, max_x), y.clamp(margin, max_y), max_height)
}

/// Seals a floating menu surface against the elements painted beneath it.
///
/// GPUI hitboxes do not implicitly occlude hitboxes behind them, so a menu
/// drawn over the path bar or the transfer list must own its pointer input:
/// otherwise a mouse-down that opens a menu item also reaches the underlying
/// `on_mouse_down` control (for example the breadcrumb children button), which
/// opens a second, unrelated menu. Keep this in sync with the path menu surface
/// in `path_bar.rs`.
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

/// The transfers panel body.
///
/// Takes no `NyaTermApp`. GPUI records every entity read during a draw, so an app
/// read here -- even a diagnostic one -- would put the panel back on the app's
/// invalidation path and undo the isolation. Everything it draws comes from the
/// snapshot; everything it changes goes back through `TransferPanel::with_app`.
pub(in crate::features::pages::transfers) fn transfer_panel(
    panel: &mut TransferPanel,
    window: &mut gpui::Window,
    cx: &mut Context<TransferPanel>,
) -> gpui::AnyElement {
    // Before the first flush there is nothing to draw and nothing to draw it from.
    let Some(snapshot) = panel.snapshot() else {
        return div().into_any_element();
    };
    let chrome = snapshot.chrome;
    let transfer_height = snapshot.panel_height;

    // Tauri AppPanelContent: FileExplorer (flex-1) + vertical resize + FileTransfer fixed height.
    div()
        .size_full()
        .flex()
        .flex_col()
        .overflow_hidden()
        .bg(chrome.transparent_surface)
        .child(
            div()
                .flex_1()
                .min_h_0()
                .overflow_hidden()
                .child(transfer_browser_view(panel, window, cx)),
        )
        .child(transfer_height_resize_handle(
            chrome,
            snapshot.height_is_resizing,
            snapshot.resize_handle_highlighted,
            cx,
        ))
        .child(
            div()
                .h(px(transfer_height))
                .flex_none()
                .overflow_hidden()
                .child(transfer_queue_view(panel, cx)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use gpui::{
        Context, InteractiveElement as _, IntoElement, Modifiers, MouseButton, ParentElement as _,
        Render, Styled as _, TestAppContext, VisualTestContext, Window, div, px,
    };

    use super::{
        TransferBrowserAvailability, menu_input_boundary, transfer_browser_availability,
        transfer_dialog_width, transfer_menu_position,
    };

    struct MenuBoundaryFixture {
        beneath_mouse_downs: Arc<AtomicUsize>,
        sealed: bool,
    }

    impl Render for MenuBoundaryFixture {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let beneath = self.beneath_mouse_downs.clone();
            let surface = div()
                .id("floating-menu")
                .absolute()
                .top(px(20.))
                .left(px(20.))
                .w(px(180.))
                .h(px(100.))
                .debug_selector(|| "floating-menu".to_string());
            let surface = if self.sealed {
                menu_input_boundary(surface)
            } else {
                surface
            };
            div()
                .size_full()
                .relative()
                .child(
                    div()
                        .id("beneath-breadcrumb-chevron")
                        .absolute()
                        .top(px(20.))
                        .left(px(20.))
                        .w(px(200.))
                        .h(px(120.))
                        .debug_selector(|| "beneath-breadcrumb-chevron".to_string())
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            beneath.fetch_add(1, Ordering::SeqCst);
                        }),
                )
                .child(surface.child(div().id("menu-item").w_full().h(px(30.))))
        }
    }

    fn click_floating_menu(cx: &mut TestAppContext, sealed: bool) -> usize {
        let beneath_mouse_downs = Arc::new(AtomicUsize::new(0));
        let fixture = MenuBoundaryFixture {
            beneath_mouse_downs: beneath_mouse_downs.clone(),
            sealed,
        };
        let (_, cx) = cx.add_window_view(|_, _| fixture);
        let cx: &mut VisualTestContext = cx;
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });

        let menu = cx.debug_bounds("floating-menu").unwrap();
        cx.simulate_mouse_down(menu.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(menu.center(), MouseButton::Left, Modifiers::default());
        beneath_mouse_downs.load(Ordering::SeqCst)
    }

    #[gpui::test]
    fn floating_menu_does_not_leak_mouse_down_to_control_beneath(cx: &mut TestAppContext) {
        assert_eq!(
            click_floating_menu(cx, true),
            0,
            "mouse-down on the sealed floating menu must not reach the control painted beneath it"
        );
    }

    #[gpui::test]
    fn unsealed_floating_menu_does_leak_mouse_down_to_control_beneath(cx: &mut TestAppContext) {
        assert_eq!(
            click_floating_menu(cx, false),
            1,
            "without an input boundary the control beneath must receive the mouse-down"
        );
    }

    #[test]
    fn browser_availability_distinguishes_session_states() {
        assert_eq!(
            transfer_browser_availability(false, false, false),
            TransferBrowserAvailability::NoSession
        );
        assert_eq!(
            transfer_browser_availability(true, false, false),
            TransferBrowserAvailability::UnsupportedSession
        );
        assert_eq!(
            transfer_browser_availability(true, true, true),
            TransferBrowserAvailability::DisconnectedSession
        );
        assert_eq!(
            transfer_browser_availability(true, true, false),
            TransferBrowserAvailability::Browsable
        );
    }

    #[test]
    fn dialog_width_uses_preferred_size_with_narrow_viewport_fallback() {
        assert_eq!(transfer_dialog_width(1280., 500.), 500.);
        assert_eq!(transfer_dialog_width(420., 500.), 388.);
        assert_eq!(transfer_dialog_width(200., 500.), 240.);
    }

    #[test]
    fn menu_position_stays_inside_viewport() {
        assert_eq!(
            transfer_menu_position(1200., 760., 268., 360., 1280., 800.),
            (1004., 432., 784.)
        );
        assert_eq!(
            transfer_menu_position(500., 500., 268., 560., 300., 240.),
            (24., 8., 224.)
        );
    }
}
