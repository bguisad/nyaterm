use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, Entity, InteractiveElement as _, IntoElement, Modifiers, MouseButton,
    ParentElement as _, Render, ScrollDelta, ScrollEvent, ScrollSource,
    StatefulInteractiveElement as _, Styled as _, TestAppContext, TouchPhase, VisualTestContext,
    Window, div, point, prelude::FluentBuilder as _, px,
};
use nyaterm_ui::{NyaScrollable, overlay::full_window_overlay_layer};

use super::menu_input_boundary;

#[derive(Default)]
struct Observations {
    down: Cell<usize>,
    up: Cell<usize>,
    moved: Cell<usize>,
    hovered: Cell<usize>,
    scrolled: Cell<usize>,
    clicked: Cell<usize>,
    item_clicks: Cell<usize>,
}

struct MenuFixture {
    observations: Rc<Observations>,
    sealed: bool,
    modal: bool,
    scrollable: bool,
    open: bool,
}

impl Render for MenuFixture {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let down = self.observations.clone();
        let left_up = self.observations.clone();
        let middle_up = self.observations.clone();
        let right_up = self.observations.clone();
        let moved = self.observations.clone();
        let hovered = self.observations.clone();
        let scrolled = self.observations.clone();
        let clicked = self.observations.clone();
        div()
            .size_full()
            .relative()
            .child(
                div()
                    .id("background")
                    .absolute()
                    .inset_0()
                    .on_any_mouse_down(move |_, _, _| down.down.set(down.down.get() + 1))
                    .on_mouse_up(MouseButton::Left, move |_, _, _| {
                        left_up.up.set(left_up.up.get() + 1);
                    })
                    .on_mouse_up(MouseButton::Middle, move |_, _, _| {
                        middle_up.up.set(middle_up.up.get() + 1);
                    })
                    .on_mouse_up(MouseButton::Right, move |_, _, _| {
                        right_up.up.set(right_up.up.get() + 1);
                    })
                    .on_mouse_move(move |_, _, _| moved.moved.set(moved.moved.get() + 1))
                    .on_hover(move |hover, _, _| {
                        if *hover {
                            hovered.hovered.set(hovered.hovered.get() + 1);
                        }
                    })
                    .on_scroll(move |_, _, _| scrolled.scrolled.set(scrolled.scrolled.get() + 1))
                    .on_click(move |_, _, _| clicked.clicked.set(clicked.clicked.get() + 1)),
            )
            .when(self.open, |root| {
                let item_clicks = self.observations.clone();
                let surface = div()
                    .id("floating-menu")
                    .debug_selector(|| "floating-menu".into())
                    .absolute()
                    .top(px(20.))
                    .left(px(20.))
                    .w(px(180.))
                    .h(px(100.));
                let surface = if self.sealed {
                    menu_input_boundary(surface)
                } else {
                    surface
                };
                let item = div()
                    .id("menu-item")
                    .debug_selector(|| "menu-item".into())
                    .w_full()
                    .h(px(30.))
                    .flex_none()
                    .on_click(move |_, _, _| {
                        item_clicks
                            .item_clicks
                            .set(item_clicks.item_clicks.get() + 1);
                    });
                let content =
                    if self.scrollable {
                        div()
                            .h_full()
                            .overflow_y_scrollbar()
                            .child(div().flex().flex_col().child(item).children((1..12).map(
                                |index| {
                                    div().h(px(30.)).flex_none().when(index == 11, |row| {
                                        row.debug_selector(|| "last-item".into())
                                    })
                                },
                            )))
                            .into_any_element()
                    } else {
                        item.into_any_element()
                    };
                let surface = surface.child(content);
                if self.modal {
                    root.child(full_window_overlay_layer(
                        "menu-input-layer",
                        div()
                            .id("scrim")
                            .absolute()
                            .inset_0()
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.open = false;
                                cx.notify();
                            }))
                            .child(surface),
                    ))
                } else {
                    root.child(surface)
                }
            })
    }
}

fn fixture(
    cx: &mut TestAppContext,
    sealed: bool,
    modal: bool,
    scrollable: bool,
) -> (
    Entity<MenuFixture>,
    &mut VisualTestContext,
    Rc<Observations>,
) {
    cx.update(nyaterm_ui::init_components);
    let observations = Rc::new(Observations::default());
    let observed = observations.clone();
    let (entity, cx) = cx.add_window_view(|_, _| MenuFixture {
        observations: observed,
        sealed,
        modal,
        scrollable,
        open: true,
    });
    draw(cx);
    // The test platform initially places the pointer outside the menu. Observe
    // hover only after moving into its covered region and settling the redraw.
    let item = cx.debug_bounds("menu-item").unwrap();
    cx.simulate_mouse_move(item.center(), None, Modifiers::none());
    draw(cx);
    observations.moved.set(0);
    observations.hovered.set(0);
    (entity, cx, observations)
}

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| _ = window.draw(cx));
}

#[gpui::test]
fn unsealed_menu_item_click_also_reaches_background(cx: &mut TestAppContext) {
    let (_, cx, observed) = fixture(cx, false, false, false);
    let item = cx.debug_bounds("menu-item").unwrap();
    cx.simulate_click(item.center(), Modifiers::none());
    assert_eq!(observed.item_clicks.get(), 1);
    assert_eq!(observed.down.get(), 1);
    assert_eq!(observed.clicked.get(), 1);
}

#[gpui::test]
fn sealed_menu_keeps_item_clicks_and_blocks_background_pointer_events(cx: &mut TestAppContext) {
    let (_, cx, observed) = fixture(cx, true, false, false);
    let item = cx.debug_bounds("menu-item").unwrap();
    for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
        cx.simulate_mouse_move(item.center(), None, Modifiers::none());
        cx.simulate_mouse_down(item.center(), button, Modifiers::none());
        cx.simulate_mouse_up(item.center(), button, Modifiers::none());
    }
    assert_eq!(observed.item_clicks.get(), 1);
    assert_eq!(observed.down.get(), 0);
    assert_eq!(observed.up.get(), 0);
    assert_eq!(observed.moved.get(), 0);
    assert_eq!(observed.hovered.get(), 0);
    assert_eq!(observed.clicked.get(), 0);
}

#[gpui::test]
fn modal_menu_keeps_inside_clicks_and_outside_click_only_dismisses(cx: &mut TestAppContext) {
    let (entity, cx, observed) = fixture(cx, true, true, false);
    let item = cx.debug_bounds("menu-item").unwrap();
    cx.simulate_click(item.center(), Modifiers::none());
    assert_eq!(observed.item_clicks.get(), 1);
    let menu = cx.debug_bounds("floating-menu").unwrap();
    cx.simulate_click(menu.center(), Modifiers::none());
    cx.update(|_, cx| assert!(entity.read(cx).open));
    let outside = point(menu.right() + px(30.), menu.center().y);
    cx.simulate_click(outside, Modifiers::none());
    cx.update(|_, cx| assert!(!entity.read(cx).open));
    assert_eq!(observed.down.get(), 0);
    assert_eq!(observed.up.get(), 0);
    assert_eq!(observed.clicked.get(), 0);
    draw(cx);
    cx.simulate_click(outside, Modifiers::none());
    assert_eq!(observed.down.get(), 1);
    assert_eq!(observed.clicked.get(), 1);
}

#[gpui::test]
fn modal_menu_scrolls_without_scrolling_background_at_its_boundary(cx: &mut TestAppContext) {
    let (_, cx, observed) = fixture(cx, true, true, true);
    let menu = cx.debug_bounds("floating-menu").unwrap();
    let last_before = cx.debug_bounds("last-item").unwrap();
    let scroll = ScrollEvent {
        position: menu.center(),
        delta: ScrollDelta::Pixels(point(px(0.), px(-1000.))),
        modifiers: Modifiers::none(),
        touch_phase: TouchPhase::Moved,
        source: ScrollSource::Pointer,
    };
    cx.simulate_event(scroll.clone());
    draw(cx);
    let last_after = cx.debug_bounds("last-item").unwrap();
    assert!(last_after.top() < last_before.top());
    assert!(menu.contains(&last_after.center()));
    cx.simulate_event(scroll);
    draw(cx);
    assert_eq!(cx.debug_bounds("last-item").unwrap(), last_after);
    assert_eq!(observed.scrolled.get(), 0);
}
