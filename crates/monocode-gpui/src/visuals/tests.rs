use super::*;
use gpui::TestAppContext;

fn document(source: &str) -> VisualDocument {
    VisualDocument {
        id: "native-test".into(),
        title: "Native visual".into(),
        source: source.into(),
    }
}
fn first_text(node: &Option<Node>) -> &str {
    match node.as_ref().unwrap() {
        Node::Column { children, .. } => match &children[0] {
            Node::Text { text, .. } => text,
            _ => panic!("Expected text"),
        },
        _ => panic!("Expected column"),
    }
}

#[gpui::test]
fn native_button_preserves_state_across_frames(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                VisualView::new(
                    document(
                        r#"
        const state = visual.state({count: 0});
        return () => visual.column([
            visual.text(String(state.count)),
            visual.button('Increase', () => { state.count++; })
        ]);
    "#,
                    ),
                    cx,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    assert!(visual.debug_bounds("visual-expand").is_none());
    for expected in ["1", "2"] {
        let bounds = visual
            .debug_bounds("visual-button-root/1")
            .expect("Native GPUI button must be present");
        visual.simulate_click(bounds.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |view, _, _| {
                assert_eq!(first_text(&view.tree), expected)
            })
            .unwrap();
    }
}

#[gpui::test]
fn native_tabs_render_and_rebind_selection_across_clicks(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                VisualView::new(
                    document(
                        r#"
        const state = visual.state({tab: 0});
        return () => visual.column([
            visual.text(String(state.tab)),
            visual.tabs(['First', 'Second', 'Third'], state.tab, value => { state.tab = value; })
        ]);
    "#,
                    ),
                    cx,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    for (selected, selector) in [
        (1, "visual-tab-root/1-1"),
        (2, "visual-tab-root/1-2"),
        (0, "visual-tab-root/1-0"),
    ] {
        let bounds = visual.debug_bounds(selector).unwrap();
        visual.simulate_click(bounds.center(), Default::default());
        visual.run_until_parked();
        window
            .update(visual, |view, _, _| {
                assert!(view.error.is_none(), "{:?}", view.error);
                assert_eq!(first_text(&view.tree), selected.to_string());
                let Some(Node::Column { children, .. }) = &view.tree else {
                    panic!("Expected column")
                };
                let Node::Tabs {
                    selected: actual, ..
                } = &children[1]
                else {
                    panic!("Expected tabs")
                };
                assert_eq!(*actual, selected);
            })
            .unwrap();
    }
}

#[gpui::test]
fn slider_drag_rebinds_callbacks_after_each_native_update(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                VisualView::new(
                    document(
                        r#"
        const state = visual.state({amount: 0});
        return () => visual.column([
            visual.text(String(state.amount)),
            visual.slider('Amount', state.amount, 0, 100, value => { state.amount = value; })
        ]);
    "#,
                    ),
                    cx,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let track = visual.debug_bounds("visual-slider-root/1").unwrap();
    visual.simulate_event(gpui::MouseDownEvent {
        position: point(track.left() + track.size.width * 0.25, track.center().y),
        button: MouseButton::Left,
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    });
    visual.run_until_parked();
    visual.simulate_event(gpui::MouseMoveEvent {
        position: point(track.left() + track.size.width * 0.75, track.center().y),
        pressed_button: Some(MouseButton::Left),
        modifiers: Default::default(),
    });
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            assert_eq!(first_text(&view.tree), "75");
        })
        .unwrap();
}

#[gpui::test]
fn failed_handler_is_contained(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                VisualView::new(
                    document(
                        r#"
        return () => visual.column([
            visual.text('Still here'),
            visual.button('Fail', () => { throw new Error('Expected failure'); })
        ]);
    "#,
                    ),
                    cx,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let fail = visual.debug_bounds("visual-button-root/1").unwrap();
    visual.simulate_click(fail.center(), Default::default());
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert!(view.error.as_deref().unwrap().contains("Expected failure"));
            assert_eq!(first_text(&view.tree), "Still here");
        })
        .unwrap();
}

#[gpui::test]
fn native_input_and_selection_keep_working_after_state_updates(cx: &mut TestAppContext) {
    cx.update(crate::input::bind);
    let window=cx.update(|cx|cx.open_window(Default::default(),|_,cx|cx.new(|cx|VisualView::new(document(r#"
        const state = visual.state({query:'',selected:0});
        return () => visual.column([
            visual.text(state.query + ':' + state.selected),
            visual.input('Filter',state.query,value=>{state.query=value;},{placeholder:'Type a filter'}),
            visual.select('Mode',['First','Second'],state.selected,value=>{state.selected=value;})
        ]);
    "#),cx))).unwrap());
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let field = visual.debug_bounds("visual-input-root/1").unwrap();
    visual.simulate_click(field.center(), Default::default());
    visual.simulate_input("a");
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            assert_eq!(first_text(&view.tree), "a:0");
        })
        .unwrap();
    visual.simulate_input("b");
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            assert_eq!(first_text(&view.tree), "ab:0");
        })
        .unwrap();
    let option = visual.debug_bounds("visual-option-root/2-1").unwrap();
    visual.simulate_click(option.center(), Default::default());
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert_eq!(first_text(&view.tree), "ab:1")
        })
        .unwrap();
    let field = visual.debug_bounds("visual-input-root/1").unwrap();
    visual.simulate_click(field.center(), Default::default());
    visual.simulate_input("c");
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert!(view.error.is_none(), "{:?}", view.error);
            assert_eq!(first_text(&view.tree), "abc:1");
        })
        .unwrap();
}

#[gpui::test]
fn native_scene_orbits_and_reuses_static_projection(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| {
                VisualView::new(
                    document(
                        r#"
        return () => visual.scene([visual.object({kind:'box'})],{height:240});
    "#,
                    ),
                    cx,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let first = window
        .update(visual, |view, _, _| {
            view.scene_cache["root"]
                .borrow()
                .as_ref()
                .unwrap()
                .projection
                .clone()
        })
        .unwrap();
    visual.refresh().unwrap();
    visual.run_until_parked();
    let second = window
        .update(visual, |view, _, _| {
            view.scene_cache["root"]
                .borrow()
                .as_ref()
                .unwrap()
                .projection
                .clone()
        })
        .unwrap();
    assert!(
        Rc::ptr_eq(&first, &second),
        "Static repaints should reuse projected geometry"
    );
    let bounds = visual.debug_bounds("visual-scene-root").unwrap();
    visual.simulate_event(gpui::MouseDownEvent {
        position: bounds.center(),
        button: MouseButton::Left,
        modifiers: Default::default(),
        click_count: 1,
        first_mouse: false,
    });
    visual.simulate_event(gpui::MouseMoveEvent {
        position: bounds.center() + point(px(60.), px(25.)),
        pressed_button: Some(MouseButton::Left),
        modifiers: Default::default(),
    });
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            let orbit = view.orbits["root"];
            assert!((orbit.yaw - 0.6).abs() < 0.001);
            assert!((orbit.pitch - 0.25).abs() < 0.001);
            let after = view.scene_cache["root"]
                .borrow()
                .as_ref()
                .unwrap()
                .projection
                .clone();
            assert!(!Rc::ptr_eq(&first, &after));
        })
        .unwrap();
}

#[gpui::test]
fn tall_visual_uses_full_height_and_wheel_scrolls_the_conversation(cx: &mut TestAppContext) {
    struct Conversation {
        view: gpui::Entity<VisualView>,
        scroll: gpui::ScrollHandle,
    }
    impl Render for Conversation {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .id("visual-test-conversation")
                .debug_selector(|| "visual-test-conversation".into())
                .size_full()
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(self.view.clone())
        }
    }
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            let view = cx.new(|cx| {
                VisualView::new(
                    document(
                        r#"
                return () => visual.column(Array.from({length: 40}, (_, i) =>
                    visual.button('Row ' + i, () => {})), {gap: 12});
            "#,
                    ),
                    cx,
                )
            });
            cx.new(|_| Conversation {
                view,
                scroll: gpui::ScrollHandle::new(),
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_window_resize(window.into(), size(px(800.), px(500.)));
    visual.run_until_parked();
    let content = visual.debug_bounds("visual-content").unwrap();
    assert!(
        content.size.height > px(1400.),
        "All forty rows must contribute to the visual's height: {content:?}"
    );
    window
        .update(visual, |conversation, _, _| {
            assert!(conversation.scroll.max_offset().y > px(900.));
            assert_eq!(conversation.scroll.offset().y, px(0.));
        })
        .unwrap();
    let viewport = visual.debug_bounds("visual-test-conversation").unwrap();
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-180.))),
        modifiers: Default::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    visual.run_until_parked();
    window
        .update(visual, |conversation, _, _| {
            assert!(
                conversation.scroll.offset().y < px(-100.),
                "Scrolling over a tall visual must move the conversation"
            );
        })
        .unwrap();
    assert_eq!(
        visual.debug_bounds("visual-content").unwrap().size.height,
        content.size.height
    );
}

#[gpui::test]
fn remote_tree_forwards_events_and_accepts_plugin_updates(cx: &mut TestAppContext) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let on_event = {
        let events = events.clone();
        Rc::new(move |handler: &str, value: Value| {
            events.borrow_mut().push((handler.to_string(), value));
        })
    };
    let tree = |text: &str| Node::Column {
        children: vec![
            Node::Text {
                text: text.into(),
                size: TextSize::Body,
                color: ThemeColor::Ink,
                monospace: false,
            },
            Node::Button {
                label: "Click".into(),
                handler: "click".into(),
                disabled: false,
            },
        ],
        gap: 8.,
        width: None,
        weight: None,
    };
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-test".into(),
                    "Remote".into(),
                    tree("Count: 0"),
                    on_event.clone(),
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let bounds = visual
        .debug_bounds("visual-button-root/1")
        .expect("remote tree should render native controls");
    visual.simulate_click(bounds.center(), Default::default());
    visual.run_until_parked();
    assert_eq!(events.borrow()[0].0, "click");
    window
        .update(visual, |view, _, cx| view.set_tree(tree("Count: 1"), cx))
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |view, _, _| {
            assert_eq!(first_text(&view.tree), "Count: 1")
        })
        .unwrap();
}

#[gpui::test]
fn remote_page_tabs_render_a_vertical_rail_and_forward_selection(cx: &mut TestAppContext) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let on_event = {
        let events = events.clone();
        Rc::new(move |handler: &str, value: Value| {
            events.borrow_mut().push((handler.to_string(), value));
        })
    };
    let tree = Node::Column {
        children: vec![Node::PageTabs {
            labels: vec!["Guide".into(), "API".into()],
            selected: 0,
            handler: "page".into(),
            nav_width: 210.,
            right_margin: false,
            children: vec![Node::Text {
                text: "Guide body".into(),
                size: TextSize::Body,
                color: ThemeColor::Ink,
                monospace: false,
            }],
        }],
        gap: 8.,
        width: None,
        weight: None,
    };
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-page-tabs-test".into(),
                    "Remote page tabs".into(),
                    tree,
                    on_event.clone(),
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let first = visual
        .debug_bounds("visual-page-tab-root/0-0")
        .expect("the first rail item should render");
    let panel = visual
        .debug_bounds("visual-page-panel-root/0")
        .expect("the page panel should render");
    let layout = visual
        .debug_bounds("visual-page-layout-root/0")
        .expect("the page layout should render");
    assert!(panel.origin.x > first.origin.x);
    assert!(
        panel.size.width.as_f32() <= 800.,
        "the content column should stay at or below 800 px"
    );
    assert!(
        (panel.center().x - layout.center().x).as_f32().abs() < 1.,
        "the content column should be centered in the page layout"
    );
    let bounds = visual
        .debug_bounds("visual-page-tab-root/0-1")
        .expect("the second rail item should render");
    visual.simulate_click(bounds.center(), Default::default());
    visual.run_until_parked();
    assert_eq!(events.borrow().len(), 1);
    assert_eq!(events.borrow()[0].0, "page");
    assert_eq!(events.borrow()[0].1, json!(1));
}

#[gpui::test]
fn remote_code_editor_renders_and_forwards_edits(cx: &mut TestAppContext) {
    cx.update(crate::input::bind);
    let events = Rc::new(RefCell::new(Vec::new()));
    let on_event = {
        let events = events.clone();
        Rc::new(move |handler: &str, value: Value| {
            events.borrow_mut().push((handler.to_string(), value));
        })
    };
    let tree = Node::Column {
        children: vec![Node::CodeEditor {
            label: "Source".into(),
            value: "let answer = 41;".into(),
            language: "rs".into(),
            height: 220.,
            handler: "source".into(),
            read_only: false,
            submit_handler: None,
            selection_handler: None,
            context_menu: Vec::new(),
        }],
        gap: 8.,
        width: None,
        weight: None,
    };
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-editor-test".into(),
                    "Remote editor".into(),
                    tree,
                    on_event.clone(),
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let bounds = visual
        .debug_bounds("visual-code-editor-root/0")
        .expect("remote code editor should render");
    visual.simulate_click(bounds.center(), Default::default());
    visual.simulate_input("2");
    visual.run_until_parked();
    assert!(
        events.borrow().iter().any(|(handler, value)| {
            handler == "source" && value.as_str().is_some_and(|text| text.contains('2'))
        }),
        "code editor edits should be forwarded: {:?}",
        events.borrow()
    );
}

#[gpui::test]
fn width_changes_emit_row_remeasure_events(cx: &mut TestAppContext) {
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|cx| VisualView::new(document("return () => visual.text('Ready');"), cx))
        })
        .unwrap()
    });
    let view = window.update(cx, |_, _, cx| cx.entity()).unwrap();
    let count = Rc::new(Cell::new(0));
    let event_count = count.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&view, move |_, _: &VisualChanged, _| {
            event_count.set(event_count.get() + 1)
        })
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    count.set(0);
    visual.simulate_window_resize(window.into(), size(px(300.), px(700.)));
    visual.refresh().unwrap();
    visual.run_until_parked();
    assert!(
        count.get() > 0,
        "Responsive width changes must also remeasure the transcript row"
    );
}

#[gpui::test]
fn remote_table_row_click_and_compact_row_buttons_work(cx: &mut TestAppContext) {
    let events = Rc::new(RefCell::new(Vec::new()));
    let on_event = {
        let events = events.clone();
        Rc::new(move |handler: &str, value: Value| {
            events.borrow_mut().push((handler.to_string(), value));
        })
    };
    let tree: Node = serde_json::from_value(json!({
        "kind": "column",
        "gap": 8,
        "children": [
            {
                "kind": "row",
                "gap": 8,
                "children": [
                    {"kind": "button", "label": "Run", "handler": "run", "disabled": false},
                    {"kind": "spacer"},
                    {"kind": "text", "text": "idle", "size": "caption", "color": "muted", "monospace": true}
                ]
            },
            {
                "kind": "table",
                "columns": ["Path", "Kind"],
                "rows": [["a.csv", "CSV"], ["b.json", "JSON"]],
                "handler": "pick"
            }
        ]
    }))
    .unwrap();
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-table-test".into(),
                    "Remote table".into(),
                    tree,
                    on_event,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_window_resize(window.into(), size(px(800.), px(500.)));
    visual.run_until_parked();
    let run_bounds = visual
        .debug_bounds("visual-button-root/0/0")
        .expect("toolbar button should render");
    assert!(
        run_bounds.size.width < px(120.),
        "row button should stay compact instead of stretching: {run_bounds:?}"
    );
    let second_row = visual
        .debug_bounds("visual-table-row-root/1-1")
        .expect("selectable table row should render");
    visual.simulate_click(second_row.center(), Default::default());
    visual.run_until_parked();
    assert_eq!(*events.borrow(), vec![("pick".to_string(), json!(1))]);
}

#[gpui::test]
fn remote_editor_preserves_inflight_keystrokes_across_delayed_echoes(cx: &mut TestAppContext) {
    cx.update(crate::input::bind);
    let make_tree = |text: &str| -> Node {
        serde_json::from_value(json!({
            "kind": "code_editor",
            "label": "Cell",
            "value": text,
            "language": "py",
            "height": 64,
            "handler": "cell"
        }))
        .unwrap()
    };
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-typing-test".into(),
                    "Remote typing".into(),
                    make_tree(""),
                    Rc::new(|_, _| {}),
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.run_until_parked();
    let bounds = visual.debug_bounds("visual-code-editor-root").unwrap();
    visual.simulate_click(bounds.center(), Default::default());
    visual.simulate_input("a");
    visual.simulate_input("b");
    visual.run_until_parked();
    // Simulate a delayed echo from the first keystroke arriving after the second.
    window
        .update(visual, |view, _, cx| view.set_tree(make_tree("a"), cx))
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |view, _, cx| {
            let content = view.editors["root"].input.read(cx).content.to_string();
            assert_eq!(content, "ab");
        })
        .unwrap();
}

#[gpui::test]
fn remote_right_margin_aligns_beside_centered_editor_and_forwards_selection(
    cx: &mut TestAppContext,
) {
    cx.update(crate::input::bind);
    let events = Rc::new(RefCell::new(Vec::new()));
    let on_event = {
        let events = events.clone();
        Rc::new(move |handler: &str, value: Value| {
            events.borrow_mut().push((handler.to_string(), value));
        })
    };
    let make_tree = |has_selection: bool| -> Node {
        serde_json::from_value(json!({
            "kind": "page_tabs",
            "labels": ["Notebook", "Variables"],
            "selected": 0,
            "handler": "tab",
            "nav_width": 180,
            "right_margin": true,
            "children": [
                {
                    "kind": "column",
                    "gap": 20,
                    "children": [
                        {
                            "kind": "row",
                            "gap": 24,
                            "children": [
                                {
                                    "kind": "column",
                                    "gap": 8,
                                    "children": [
                                        {
                                            "kind": "code_editor",
                                            "label": "Block 1",
                                            "value": "x = 2\ny = x * 10\ny",
                                            "language": "py",
                                            "height": 180,
                                            "handler": "cell:1",
                                            "submit_handler": "run:1",
                                            "selection_handler": "select:1"
                                        }
                                    ]
                                },
                                {
                                    "kind": "column",
                                    "gap": 6,
                                    "width": 180,
                                    "children": [
                                        {
                                            "kind": "button",
                                            "label": "Run selection",
                                            "handler": "run_sel:1",
                                            "disabled": !has_selection
                                        },
                                        {
                                            "kind": "button",
                                            "label": "Run block",
                                            "handler": "run:1",
                                            "disabled": false
                                        }
                                    ]
                                }
                            ]
                        }
                    ]
                }
            ]
        }))
        .unwrap()
    };
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-right-margin-test".into(),
                    "Notebook".into(),
                    make_tree(false),
                    on_event,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_window_resize(window.into(), size(px(1300.), px(700.)));
    visual.run_until_parked();

    let layout = visual.debug_bounds("visual-page-layout-root").unwrap();
    let editor = visual
        .debug_bounds("visual-code-editor-root/0/0/0/0")
        .unwrap();
    let run_sel = visual.debug_bounds("visual-button-root/0/0/1/0").unwrap();
    assert!(
        (editor.size.width.as_f32() - 800.).abs() < 1.5,
        "editor must keep the 800 px centered column width: {editor:?}"
    );
    assert!(
        (editor.center().x - layout.center().x).as_f32().abs() < 1.5,
        "editor must remain centered in the page layout: editor={editor:?} layout={layout:?}"
    );
    assert!(
        run_sel.origin.x >= editor.right() + px(20.),
        "controls must sit in the right margin: run_sel={run_sel:?} editor={editor:?}"
    );
    assert!(
        (run_sel.origin.y - editor.origin.y).as_f32().abs() < 4.,
        "controls must align with the top of the block: run_sel={run_sel:?} editor={editor:?}"
    );

    window
        .update(visual, |view, _, cx| {
            let input = view.editors["root/0/0/0/0"].input.clone();
            input.update(cx, |input, cx| input.select_range(6..16, cx));
        })
        .unwrap();
    visual.run_until_parked();
    assert_eq!(
        *events.borrow(),
        vec![("select:1".to_string(), json!("y = x * 10"))]
    );

    // Republishing the tree to enable the button must preserve the active selection.
    window
        .update(visual, |view, _, cx| view.set_tree(make_tree(true), cx))
        .unwrap();
    visual.run_until_parked();
    window
        .update(visual, |view, _, cx| {
            let selected = view.editors["root/0/0/0/0"].input.read(cx).selected_text();
            assert_eq!(selected, "y = x * 10");
        })
        .unwrap();

    let run_sel = visual.debug_bounds("visual-button-root/0/0/1/0").unwrap();
    visual.simulate_click(run_sel.center(), Default::default());
    visual.run_until_parked();
    assert_eq!(
        events.borrow().last(),
        Some(&("run_sel:1".to_string(), Value::Null))
    );
}

#[gpui::test]
fn remote_three_column_layout_and_editor_context_menu_work(cx: &mut TestAppContext) {
    cx.update(crate::input::bind);
    cx.update(crate::popover::bind);
    let events = Rc::new(RefCell::new(Vec::new()));
    let on_event = {
        let events = events.clone();
        Rc::new(move |handler: &str, value: Value| {
            events.borrow_mut().push((handler.to_string(), value));
        })
    };
    let make_tree = |run_sel_handler: &str| -> Node {
        serde_json::from_value(json!({
            "kind": "row",
            "gap": 18,
            "children": [
                {
                    "kind": "column",
                    "gap": 8,
                    "weight": 0.95,
                    "children": [
                        {"kind": "table", "columns": ["Name", "Value"], "rows": [["x", "2"]]}
                    ]
                },
                {
                    "kind": "column",
                    "gap": 8,
                    "weight": 1.8,
                    "children": [
                        {
                            "kind": "code_editor",
                            "label": "Notebook",
                            "value": "x = 2\ny = x * 10\ny",
                            "language": "py",
                            "height": 720,
                            "handler": "edit",
                            "selection_handler": "select",
                            "context_menu": [
                                {
                                    "label": "Run selected",
                                    "handler": run_sel_handler,
                                    "requires_selection": true
                                },
                                {
                                    "label": "Run all",
                                    "handler": "run_all"
                                }
                            ]
                        }
                    ]
                },
                {
                    "kind": "column",
                    "gap": 8,
                    "weight": 1.25,
                    "children": [
                        {"kind": "button", "label": "Run all", "handler": "run_all", "disabled": false}
                    ]
                }
            ]
        }))
        .unwrap()
    };
    let window = cx.update(|cx| {
        cx.open_window(Default::default(), |_, cx| {
            cx.new(|_| {
                VisualView::remote(
                    "remote-three-col-test".into(),
                    "Notebook".into(),
                    make_tree("run_sel_v1"),
                    on_event,
                )
            })
        })
        .unwrap()
    });
    let visual = &mut gpui::VisualTestContext::from_window(window.into(), cx);
    visual.simulate_window_resize(window.into(), size(px(1200.), px(820.)));
    visual.run_until_parked();

    let table = visual.debug_bounds("visual-table-root/0/0").unwrap();
    let editor = visual.debug_bounds("visual-code-editor-root/1/0").unwrap();
    let run_btn = visual.debug_bounds("visual-button-root/2/0").unwrap();
    assert!(
        table.right() < editor.left() && editor.right() < run_btn.left(),
        "all 3 columns must sit side-by-side: table={table:?} editor={editor:?} run_btn={run_btn:?}"
    );
    assert!(
        editor.size.width > table.size.width,
        "center code sheet must be wider than variables column: editor={editor:?} table={table:?}"
    );

    window
        .update(visual, |view, _, cx| {
            let input = view.editors["root/1/0"].input.clone();
            input.update(cx, |input, cx| input.select_range(6..16, cx));
        })
        .unwrap();
    visual.run_until_parked();

    // Right-click inside the editor to open the native context menu.
    visual.simulate_mouse_down(editor.center(), MouseButton::Right, Default::default());
    visual.run_until_parked();

    // Simulate a concurrent ui.set arriving while the menu is open (rewriting handler IDs).
    window
        .update(visual, |view, _, cx| view.set_tree(make_tree("run_sel_v2"), cx))
        .unwrap();
    visual.run_until_parked();

    let menu_item = visual
        .debug_bounds("menu-action-0")
        .expect("context menu action should render");
    visual.simulate_click(menu_item.center(), Default::default());
    visual.run_until_parked();

    assert_eq!(
        events.borrow().last(),
        Some(&("run_sel_v2".to_string(), json!("y = x * 10")))
    );
}
