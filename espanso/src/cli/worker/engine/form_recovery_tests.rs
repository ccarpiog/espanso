use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque},
    path::Path,
};

use super::*;
use crate::{
    cli::worker::engine::dispatch::executor::clipboard_injector::ClipboardParams,
    gui::recovery::{FormAction, LastForm},
};
use espanso_config::matches::{Match, TextEffect};
use espanso_engine::event::effect::{
    HtmlInjectRequest, ImageInjectRequest, KeySequenceInjectRequest, MarkdownInjectRequest,
    TextInjectRequest,
};

#[derive(Default)]
struct FakeClipboard {
    copies: RefCell<Vec<SavedResult>>,
    fail: Cell<bool>,
}

impl Clipboard for FakeClipboard {
    fn get_text(&self, _: &ClipboardOperationOptions) -> Option<String> {
        panic!("explicit Copy must not restore the previous clipboard")
    }
    fn set_text(&self, text: &str, _: &ClipboardOperationOptions) -> Result<()> {
        self.copies.borrow_mut().push(SavedResult {
            text: text.into(),
            html: None,
        });
        if self.fail.get() {
            anyhow::bail!("simulated clipboard failure");
        }
        Ok(())
    }
    fn set_html(
        &self,
        html: &str,
        fallback: Option<&str>,
        _: &ClipboardOperationOptions,
    ) -> Result<()> {
        self.copies.borrow_mut().push(SavedResult {
            text: fallback.unwrap_or_default().into(),
            html: Some(html.into()),
        });
        if self.fail.get() {
            anyhow::bail!("simulated clipboard failure");
        }
        Ok(())
    }
    fn set_image(&self, _: &Path, _: &ClipboardOperationOptions) -> Result<()> {
        panic!("form completion must not copy an unrelated image")
    }
}

#[derive(Default)]
struct FakeUI(RefCell<Vec<String>>);
impl UIRemote for FakeUI {
    fn update_tray_icon(&self, _: espanso_ui::icons::TrayIcon) {}
    fn show_notification(&self, message: &str) {
        self.0.borrow_mut().push(message.into());
    }
    fn show_context_menu(&self, _: &espanso_ui::menu::Menu) {}
    fn exit(&self) {}
}

struct Params;
impl ClipboardParamsProvider for Params {
    fn get(&self) -> ClipboardParams {
        ClipboardParams {
            pre_paste_delay: 0,
            paste_shortcut_event_delay: 0,
            paste_shortcut: None,
            disable_x11_fast_inject: false,
            restore_clipboard: true,
            restore_clipboard_delay: 0,
            x11_use_xclip_backend: false,
            x11_use_xdotool_backend: false,
        }
    }
}

struct Sequence(Cell<u32>);
impl EventSequenceProvider for Sequence {
    fn get_next_id(&self) -> u32 {
        let value = self.0.get();
        self.0.set(value + 1);
        value
    }
}

struct MatchProviderRef<'a>(&'a [Match]);
impl<'a> MatchProvider<'a> for MatchProviderRef<'a> {
    fn matches(&self) -> Vec<&'a Match> {
        self.0.iter().collect()
    }
    fn get(&self, id: i32) -> Option<&'a Match> {
        self.0.iter().find(|m| m.id == id)
    }
}

#[derive(Clone, Copy)]
enum Outcome {
    Copy,
    Insert,
    Cancel,
    RenderFailure,
    Normal,
}

struct FakeProcessor<'a> {
    recovery: &'a FormRecovery,
    last: LastForm,
    outcomes: VecDeque<Outcome>,
    seen: Vec<Event>,
}

impl Processor for FakeProcessor<'_> {
    fn process(&mut self, event: Event) -> Vec<Event> {
        self.seen.push(event.clone());
        if matches!(
            event.etype,
            EventType::FormInteractionEnded(_)
                | EventType::ReopenLastForm
                | EventType::CopyLastFormResult
        ) {
            return vec![event];
        }
        let outcome = self
            .outcomes
            .pop_front()
            .expect("unexpected processing pass");
        if matches!(outcome, Outcome::Normal) {
            assert!(
                !self.recovery.used(),
                "previous form state leaked into the next expansion"
            );
            assert!(self.recovery.take_completed().is_none());
            return vec![Event::caused_by(
                event.source_id,
                EventType::TextInject(TextInjectRequest {
                    text: "normal expansion".into(),
                    ..Default::default()
                }),
            )];
        }
        self.recovery.prevent_target_capture_for_test();
        self.recovery.prepare(self.last.clone());
        self.recovery.open_form("stable schema").unwrap();
        match outcome {
            Outcome::Copy | Outcome::Insert => {
                self.recovery.select(if matches!(outcome, Outcome::Copy) {
                    FormAction::Copy
                } else {
                    FormAction::Insert
                });
                self.recovery
                    .finish(
                        SavedResult {
                            text: "completed form".into(),
                            html: None,
                        },
                        0,
                    )
                    .unwrap();
            }
            Outcome::Cancel => self.recovery.select(FormAction::Cancel),
            Outcome::RenderFailure => {}
            Outcome::Normal => unreachable!(),
        }
        if matches!(event.etype, EventType::RenderingRequested(_)) {
            // A direct reopen render has no trigger-compensation stage.
            return vec![Event::caused_by(event.source_id, EventType::NOOP)];
        }
        queued_edits(event.source_id)
    }
}

fn queued_edits(source: u32) -> Vec<Event> {
    vec![
        EventType::KeySequenceInject(KeySequenceInjectRequest {
            keys: vec![Key::Backspace; 4],
        }),
        EventType::KeySequenceInject(KeySequenceInjectRequest {
            keys: vec![Key::ArrowLeft],
        }),
        EventType::TextInject(TextInjectRequest {
            text: "render error text".into(),
            ..Default::default()
        }),
        EventType::HtmlInject(HtmlInjectRequest {
            html: "<b>unexpected</b>".into(),
        }),
        EventType::MarkdownInject(MarkdownInjectRequest {
            markdown: "**unexpected**".into(),
        }),
        EventType::ImageInject(ImageInjectRequest {
            image_path: "unused.png".into(),
        }),
        EventType::RenderingError,
    ]
    .into_iter()
    .map(|event| Event::caused_by(source, event))
    .collect()
}

struct Environment {
    _directory: tempfile::TempDir,
    recovery: FormRecovery,
    clipboard: FakeClipboard,
    ui: FakeUI,
    matches: Vec<Match>,
}

impl Environment {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let recovery = FormRecovery::new(directory.path());
        Self {
            _directory: directory,
            recovery,
            clipboard: FakeClipboard::default(),
            ui: FakeUI::default(),
            matches: vec![Match {
                id: 42,
                effect: MatchEffect::Text(TextEffect::default()),
                ..Default::default()
            }],
        }
    }
    fn last(&self) -> LastForm {
        LastForm {
            fingerprint: fingerprint(&self.matches[0]),
            context_key: "original context".into(),
            trigger: Some(":old".into()),
            args: HashMap::from([("customer".into(), "retained".into())]),
        }
    }
    fn run(&self, outcomes: &[Outcome], inputs: Vec<Event>) -> (Vec<Vec<Event>>, Vec<Event>) {
        let mut inner = FakeProcessor {
            recovery: &self.recovery,
            last: self.last(),
            outcomes: outcomes.iter().copied().collect(),
            seen: Vec::new(),
        };
        let sequence = Sequence(Cell::new(100));
        let matches = MatchProviderRef(&self.matches);
        let mut processor = RecoveryProcessor {
            inner: &mut inner,
            recovery: &self.recovery,
            clipboard: &self.clipboard,
            params: &Params,
            sequence: &sequence,
            matches: &matches,
            ui: &self.ui,
        };
        let outputs = inputs
            .into_iter()
            .map(|event| processor.process(event))
            .collect();
        (outputs, inner.seen)
    }
}

fn assert_flushed(seen: &[Event]) {
    assert!(seen.iter().any(|event| matches!(&event.etype, EventType::FormInteractionEnded(ended) if ended.minimum_source_id == 100)));
}

#[test]
fn copy_cancel_and_render_failure_suppress_every_queued_edit_and_flush_input() {
    for outcome in [Outcome::Copy, Outcome::Cancel, Outcome::RenderFailure] {
        let env = Environment::new();
        let (output, seen) = env.run(&[outcome], vec![Event::caused_by(1, EventType::Heartbeat)]);
        assert!(output[0]
            .iter()
            .all(|event| !is_editing_effect(&event.etype)));
        assert_flushed(&seen);
        assert_eq!(
            env.clipboard.copies.borrow().len(),
            usize::from(matches!(outcome, Outcome::Copy))
        );
        assert!(env.recovery.take_completed().is_none());
    }
}

#[test]
fn clipboard_failure_never_leaks_deletion_and_result_can_be_copied_again() {
    let env = Environment::new();
    env.clipboard.fail.set(true);
    let (output, seen) = env.run(
        &[Outcome::Insert],
        vec![Event::caused_by(1, EventType::Heartbeat)],
    );
    assert!(output[0]
        .iter()
        .all(|event| !is_editing_effect(&event.etype)));
    assert_flushed(&seen);
    assert_eq!(env.recovery.last_result().unwrap().text, "completed form");
    assert!(env
        .ui
        .0
        .borrow()
        .iter()
        .any(|message| message.contains("Could not copy")));
    env.clipboard.fail.set(false);
    env.run(
        &[],
        vec![Event::caused_by(2, EventType::CopyLastFormResult)],
    );
    assert_eq!(env.clipboard.copies.borrow().len(), 2);
    assert!(env
        .ui
        .0
        .borrow()
        .iter()
        .any(|message| message == "Last form result copied."));
}

#[test]
fn following_normal_expansion_is_not_suppressed_by_form_session_state() {
    let env = Environment::new();
    let (output, seen) = env.run(
        &[Outcome::Copy, Outcome::Normal],
        vec![
            Event::caused_by(1, EventType::Heartbeat),
            Event::caused_by(101, EventType::Heartbeat),
        ],
    );
    assert!(
        matches!(&output[1][0].etype, EventType::TextInject(text) if text.text == "normal expansion")
    );
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event.etype, EventType::FormInteractionEnded(_)))
            .count(),
        1
    );
}

#[test]
fn reopen_directly_renders_with_saved_args_and_never_replays_trigger_deletion() {
    let env = Environment::new();
    env.run(
        &[Outcome::Cancel],
        vec![Event::caused_by(1, EventType::Heartbeat)],
    );
    let (output, seen) = env.run(
        &[Outcome::Copy],
        vec![Event::caused_by(2, EventType::ReopenLastForm)],
    );
    let rendering = seen
        .iter()
        .find_map(|event| {
            if let EventType::RenderingRequested(render) = &event.etype {
                Some(render)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(rendering.match_id, 42);
    assert_eq!(rendering.trigger.as_deref(), Some(":old"));
    assert_eq!(
        rendering.trigger_args.get("customer").map(String::as_str),
        Some("retained")
    );
    assert!(rendering.left_separator.is_none() && rendering.right_separator.is_none());
    assert!(!seen.iter().any(|event| matches!(
        event.etype,
        EventType::MatchSelected(_) | EventType::TriggerCompensation(_)
    )));
    assert_eq!(deletion_count(&output[0]), Some(0));
    assert!(output[0]
        .iter()
        .all(|event| !is_editing_effect(&event.etype)));
    assert_flushed(&seen);
}

#[test]
fn changed_snippet_cannot_be_reopened_or_delete_text() {
    let mut env = Environment::new();
    env.run(
        &[Outcome::Cancel],
        vec![Event::caused_by(1, EventType::Heartbeat)],
    );
    env.matches[0].label = Some("changed".into());
    let (output, seen) = env.run(&[], vec![Event::caused_by(2, EventType::ReopenLastForm)]);
    assert!(!seen
        .iter()
        .any(|event| matches!(event.etype, EventType::RenderingRequested(_))));
    assert!(output[0]
        .iter()
        .all(|event| !is_editing_effect(&event.etype)));
    assert!(env
        .ui
        .0
        .borrow()
        .iter()
        .any(|message| message.contains("Cannot reopen")));
    assert_flushed(&seen);
}
