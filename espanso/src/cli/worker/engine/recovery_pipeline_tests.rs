//! Exercise the real search -> selection -> rendering -> recovery pipeline.
//! Only UI, clipboard, destination and passive configuration providers are faked.
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use espanso_clipboard::{Clipboard, ClipboardOperationOptions};
use espanso_config::{
    config::{Config, ConfigStore},
    matches::{store::MatchSet, Match, MatchEffect, TextEffect, Value, Variable},
};
use espanso_engine::{
    event::{
        effect::TextInjectMode,
        input::Key,
        internal::{DetectedMatch, MatchesDetectedEvent},
        Event, EventType,
    },
    process::*,
};

use super::{
    dispatch::executor::clipboard_injector::{ClipboardParams, ClipboardParamsProvider},
    form_recovery::{FormDestination, RecoveryProcessor},
    process::middleware::{
        multiplex::{self, MultiplexAdapter},
        render::{self, extension::form::FormProviderAdapter, RendererAdapter},
    },
};
use crate::{
    cli::worker::{
        builtin::BuiltInMatch,
        context::{AppInfoContext, ConfigContext, Context},
    },
    gui::{
        recovery::{FormAction, FormRecovery, SavedResult},
        FormField, FormUI,
    },
};

const FORM: i32 = 1;
const SEARCH: i32 = 2;
const REOPEN: i32 = 3;

struct Fixture {
    config: Arc<dyn Config>,
    form: Match,
    builtins: Vec<BuiltInMatch>,
    selected: Cell<i32>,
    sequence: Cell<u32>,
}

impl Fixture {
    fn new(directory: &Path) -> Self {
        std::fs::write(directory.join("default.yml"), "{}\n").unwrap();
        let (store, errors) = espanso_config::config::load_store(directory).unwrap();
        assert!(errors.is_empty());
        Self {
            config: store.default(),
            form: Match {
                id: FORM,
                effect: MatchEffect::Text(TextEffect {
                    replace: "Hello {{form.name}}".into(),
                    vars: vec![Variable {
                        id: 10,
                        name: "form".into(),
                        var_type: "form".into(),
                        params: [("layout".into(), Value::String("Name: [[name]]".into()))].into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
            builtins: vec![
                BuiltInMatch {
                    id: SEARCH,
                    triggers: vec!["jkj".into()],
                    action: |_| EventType::ShowSearchBar,
                    ..Default::default()
                },
                BuiltInMatch {
                    id: REOPEN,
                    action: |_| EventType::ReopenLastForm,
                    ..Default::default()
                },
            ],
            selected: Cell::new(FORM),
            sequence: Cell::new(100),
        }
    }
}
impl Context for Fixture {}
impl ConfigContext for Fixture {
    fn get_active_config(&self) -> Arc<dyn Config> {
        self.config.clone()
    }
}
impl AppInfoContext for Fixture {
    fn get_active_app_info(&self) -> espanso_info::AppInfo {
        espanso_info::AppInfo {
            title: None,
            exec: None,
            class: None,
        }
    }
}
impl<'a> multiplex::MatchProvider<'a> for &'a Fixture {
    fn get(&self, id: i32) -> Option<multiplex::MatchResult<'a>> {
        if id == FORM {
            Some(multiplex::MatchResult::User(&self.form))
        } else {
            self.builtins
                .iter()
                .find(|m| m.id == id)
                .map(multiplex::MatchResult::Builtin)
        }
    }
}
impl<'a> render::MatchProvider<'a> for &'a Fixture {
    fn matches(&self) -> Vec<&'a Match> {
        vec![&self.form]
    }
    fn get(&self, id: i32) -> Option<&'a Match> {
        (id == FORM).then_some(&self.form)
    }
}
impl render::ConfigProvider<'_> for Fixture {
    fn configs(&self) -> Vec<(Arc<dyn Config>, MatchSet<'_>)> {
        vec![self.active()]
    }
    fn active(&self) -> (Arc<dyn Config>, MatchSet<'_>) {
        (
            self.config.clone(),
            MatchSet {
                matches: vec![&self.form],
                global_vars: vec![],
            },
        )
    }
}
impl MatchFilter for Fixture {
    fn filter_active(&self, ids: &[i32]) -> Vec<i32> {
        ids.to_vec()
    }
}
impl MatchSelector for Fixture {
    fn select(&self, ids: &[i32], is_search: bool) -> Option<i32> {
        assert!(is_search);
        assert!(ids.contains(&self.selected.get()));
        Some(self.selected.get())
    }
}
impl MatchInfoProvider for Fixture {
    fn get_force_mode(&self, _: i32) -> Option<TextInjectMode> {
        None
    }
}
impl ModifierStatusProvider for Fixture {
    fn is_any_conflicting_modifier_pressed(&self) -> bool {
        false
    }
}
impl EventSequenceProvider for Fixture {
    fn get_next_id(&self) -> u32 {
        let id = self.sequence.get();
        self.sequence.set(id + 1);
        id
    }
}
impl PathProvider for Fixture {
    fn get_config_path(&self) -> &Path {
        Path::new(".")
    }
}
impl ConfigPathProvider for Fixture {
    fn get_config_path(&self) -> &Path {
        Path::new(".")
    }
}
impl MatcherMiddlewareConfigProvider for Fixture {
    fn max_history_size(&self) -> usize {
        10
    }
}
impl espanso_engine::process::MatchProvider for Fixture {
    fn get_all_matches_ids(&self) -> Vec<i32> {
        vec![FORM, SEARCH, REOPEN]
    }
}
impl UndoEnabledProvider for Fixture {
    fn is_undo_enabled(&self) -> bool {
        true
    }
}
impl EnabledStatusProvider for Fixture {
    fn is_config_enabled(&self) -> bool {
        true
    }
}
impl ModifierStateProvider for Fixture {
    fn get_modifier_state(&self) -> ModifierState {
        ModifierState {
            is_ctrl_down: false,
            is_alt_down: false,
            is_meta_down: false,
        }
    }
}
impl MatchResolver for Fixture {
    fn find_matches_from_trigger(&self, _: &str) -> Vec<DetectedMatch> {
        panic!("not a programmatic match request")
    }
}
impl NotificationManager for Fixture {
    fn notify_status_change(&self, _: bool) {
        panic!("unexpected status change")
    }
    fn notify_rendering_error(&self) {
        panic!("real form renderer failed")
    }
}
impl AltCodeSynthEnabledProvider for Fixture {
    fn is_alt_code_synthesizer_enabled(&self) -> bool {
        false
    }
}
impl ClipboardParamsProvider for Fixture {
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

struct Dialog<'a> {
    recovery: &'a FormRecovery,
    action: Cell<FormAction>,
}
impl FormUI for Dialog<'_> {
    fn show(
        &self,
        layout: &str,
        _: &HashMap<String, FormField>,
    ) -> Result<Option<HashMap<String, String>>> {
        assert_eq!(layout, "Name: [[name]]");
        self.recovery.open_form("name:text")?;
        self.recovery.select(self.action.get());
        Ok((self.action.get() != FormAction::Cancel)
            .then(|| [("name".into(), "Ada".into())].into()))
    }
}
#[derive(Default)]
struct FakeClipboard(RefCell<Vec<String>>);
impl Clipboard for FakeClipboard {
    fn get_text(&self, _: &ClipboardOperationOptions) -> Option<String> {
        panic!("must not restore clipboard")
    }
    fn set_text(&self, text: &str, _: &ClipboardOperationOptions) -> Result<()> {
        self.0.borrow_mut().push(text.into());
        Ok(())
    }
    fn set_html(&self, _: &str, _: Option<&str>, _: &ClipboardOperationOptions) -> Result<()> {
        panic!("plain form result expected")
    }
    fn set_image(&self, _: &Path, _: &ClipboardOperationOptions) -> Result<()> {
        panic!("plain form result expected")
    }
}
#[derive(Default)]
struct Destination {
    restores: Cell<usize>,
    captures: Cell<usize>,
    pastes: RefCell<Vec<(String, usize, u32)>>,
}
impl FormDestination for Destination {
    fn restore(&self) -> bool {
        self.restores.set(self.restores.get() + 1);
        true
    }
    fn capture_clipboard(&self, expected: &SavedResult) -> bool {
        assert_eq!(expected.text, "Hello Ada");
        self.captures.set(self.captures.get() + 1);
        true
    }
    fn paste(&self, deletion: &str, moves: usize, delay: u32) -> bool {
        self.pastes
            .borrow_mut()
            .push((deletion.into(), moves, delay));
        true
    }
}
#[derive(Default)]
struct Notifications(RefCell<Vec<String>>);
impl espanso_ui::UIRemote for Notifications {
    fn update_tray_icon(&self, _: espanso_ui::icons::TrayIcon) {}
    fn show_notification(&self, text: &str) {
        self.0.borrow_mut().push(text.into());
    }
    fn show_context_menu(&self, _: &espanso_ui::menu::Menu) {}
    fn exit(&self) {}
}

// Disable the native OS capture boundary after each RecoveryProcessor reset;
// record exactly what the unchanged real processor would send to the dispatcher.
struct ObservedProcessor<'a> {
    inner: &'a mut dyn Processor,
    recovery: &'a FormRecovery,
    planned: &'a RefCell<Vec<Event>>,
}
impl Processor for ObservedProcessor<'_> {
    fn process(&mut self, event: Event) -> Vec<Event> {
        self.recovery.prevent_target_capture_for_test();
        let events = self.inner.process(event);
        self.planned.borrow_mut().extend(events.iter().cloned());
        events
    }
}
fn detected(source_id: u32, id: i32, trigger: &str) -> Event {
    Event::caused_by(
        source_id,
        EventType::MatchesDetected(MatchesDetectedEvent {
            matches: vec![DetectedMatch {
                id,
                trigger: Some(trigger.into()),
                ..Default::default()
            }],
            is_search: false,
        }),
    )
}
fn assert_no_editing(events: &[Event]) {
    assert!(events.iter().all(|event| !matches!(
        event.etype,
        EventType::KeySequenceInject(_)
            | EventType::TextInject(_)
            | EventType::HtmlInject(_)
            | EventType::MarkdownInject(_)
            | EventType::ImageInject(_)
    )));
}
fn planned_backspaces(events: &[Event]) -> usize {
    events
        .iter()
        .filter_map(|event| match &event.etype {
            EventType::KeySequenceInject(keys) => {
                assert!(keys.keys.iter().all(|key| *key == Key::Backspace));
                Some(keys.keys.len())
            }
            _ => None,
        })
        .sum()
}

fn exercise_search(action: FormAction, reopen: bool) {
    let directory = tempfile::tempdir().unwrap();
    let fixture = Fixture::new(directory.path());
    let recovery = FormRecovery::new(directory.path());
    let dialog = Dialog {
        recovery: &recovery,
        action: Cell::new(action),
    };
    let form_provider = FormProviderAdapter::new(&dialog);
    let extension = espanso_render::extension::form::FormExtension::new(&form_provider);
    let renderer = espanso_render::create(vec![&extension]);
    let provider = &fixture;
    let renderer = RendererAdapter::new(&provider, &fixture, &renderer, &recovery);
    let multiplex = MultiplexAdapter::new(&provider, &fixture, &recovery);
    let matchers: &[&dyn Matcher<'_, ()>] = &[];
    let mut engine = espanso_engine::process::default(
        matchers,
        &fixture,
        &fixture,
        &multiplex,
        &renderer,
        &fixture,
        &fixture,
        &fixture,
        &fixture,
        &fixture,
        DisableOptions {
            toggle_key: None,
            toggle_key_variant: None,
            toggle_key_maximum_window: Duration::ZERO,
        },
        &fixture,
        &fixture,
        &fixture,
        &fixture,
        &fixture,
        &fixture,
        &fixture,
        &fixture,
    );
    let planned = RefCell::new(Vec::new());
    let mut observed = ObservedProcessor {
        inner: &mut engine,
        recovery: &recovery,
        planned: &planned,
    };
    let clipboard = FakeClipboard::default();
    let destination = Destination::default();
    let ui = Notifications::default();
    let mut processor = RecoveryProcessor {
        inner: &mut observed,
        recovery: &recovery,
        destination: &destination,
        clipboard: &clipboard,
        params: &fixture,
        sequence: &fixture,
        matches: &provider,
        ui: &ui,
    };
    if reopen {
        // Establish a last form using the real render pipeline and a different trigger.
        dialog.action.set(FormAction::Copy);
        assert_no_editing(&processor.process(detected(
            fixture.get_next_id(),
            FORM,
            ":previous-form",
        )));
        assert_eq!(
            recovery.last_form().unwrap().trigger.as_deref(),
            Some(":previous-form")
        );
        planned.borrow_mut().clear();
        clipboard.0.borrow_mut().clear();
        ui.0.borrow_mut().clear();
        dialog.action.set(action);
        fixture.selected.set(REOPEN);
    }
    // This is the matcher output for a typed search trigger, not a search hotkey.
    let events = processor.process(detected(fixture.get_next_id(), SEARCH, "jkj"));
    let backspaces = planned_backspaces(&planned.borrow());
    assert_eq!(
        backspaces, 3,
        "real cause/action middleware must compensate the typed search trigger"
    );
    assert_eq!(
        recovery.expected_deletion(backspaces).as_deref(),
        Some("jkj")
    );
    assert_eq!(
        recovery.last_form().unwrap().trigger.as_deref(),
        if reopen { Some(":previous-form") } else { None }
    );
    assert_no_editing(&events);
    match action {
        FormAction::Cancel => {
            assert!(clipboard.0.borrow().is_empty());
            assert_eq!(destination.restores.get(), 0);
            assert_eq!(destination.captures.get(), 0);
            assert!(destination.pastes.borrow().is_empty());
        }
        FormAction::Copy => {
            assert_eq!(&*clipboard.0.borrow(), &["Hello Ada"]);
            assert_eq!(destination.restores.get(), 0);
            assert_eq!(destination.captures.get(), 0);
            assert!(destination.pastes.borrow().is_empty());
        }
        FormAction::Insert => {
            assert_eq!(&*clipboard.0.borrow(), &["Hello Ada"]);
            assert_eq!(destination.restores.get(), 1);
            assert_eq!(destination.captures.get(), 1);
            assert_eq!(&*destination.pastes.borrow(), &[("jkj".into(), 0, 0)]);
            assert!(
                ui.0.borrow().is_empty(),
                "successful insertion must not degrade to manual paste"
            );
        }
    }
}

#[test]
fn typed_search_form_copy_suppresses_all_editing() {
    exercise_search(FormAction::Copy, false);
}
#[test]
fn typed_search_form_cancel_suppresses_all_editing() {
    exercise_search(FormAction::Cancel, false);
}
#[test]
fn typed_search_form_insert_replaces_search_trigger() {
    exercise_search(FormAction::Insert, false);
}
#[test]
fn typed_search_reopen_replaces_current_search_trigger_not_previous_form_trigger() {
    exercise_search(FormAction::Insert, true);
}
