use std::cell::RefCell;

use super::{
    discard::EventsDiscardMiddleware,
    matcher::{
        MatchResult, Matcher, MatcherEvent, MatcherMiddleware, MatcherMiddlewareConfigProvider,
        ModifierState, ModifierStateProvider,
    },
    undo::{UndoEnabledProvider, UndoMiddleware},
};
use crate::{
    event::{
        effect::TriggerCompensationEvent,
        input::{Key, KeyboardEvent, Status},
        internal::{DiscardBetweenEvent, FormInteractionEndedEvent, RenderedEvent, TextFormat},
        Event, EventType,
    },
    process::Middleware,
};

struct Options;

impl MatcherMiddlewareConfigProvider for Options {
    fn max_history_size(&self) -> usize {
        10
    }
}

impl ModifierStateProvider for Options {
    fn get_modifier_state(&self) -> ModifierState {
        ModifierState {
            is_ctrl_down: false,
            is_alt_down: false,
            is_meta_down: false,
        }
    }
}

impl UndoEnabledProvider for Options {
    fn is_undo_enabled(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct RecordingMatcher {
    previous_states: RefCell<Vec<Option<usize>>>,
}

impl Matcher<'_, usize> for RecordingMatcher {
    fn process(&self, prev_state: Option<&usize>, _: &MatcherEvent) -> (usize, Vec<MatchResult>) {
        self.previous_states.borrow_mut().push(prev_state.copied());
        (prev_state.copied().unwrap_or_default() + 1, Vec::new())
    }
}

fn key(source_id: u32, key: Key) -> Event {
    Event::caused_by(
        source_id,
        EventType::Keyboard(KeyboardEvent {
            key,
            value: Some("x".to_owned()),
            status: Status::Pressed,
            variant: None,
        }),
    )
}

fn ended(source_id: u32, minimum_source_id: u32) -> Event {
    Event::caused_by(
        source_id,
        EventType::FormInteractionEnded(FormInteractionEndedEvent { minimum_source_id }),
    )
}

fn run(middleware: &[&dyn Middleware], mut event: Event) -> Event {
    for middleware in middleware {
        event = middleware.next(event, &mut |_| {
            panic!("form completion must not synthesize injection or stats events")
        });
        if matches!(event.etype, EventType::NOOP) {
            break;
        }
    }
    event
}

#[test]
fn form_completion_drops_queued_input_and_restarts_matching_without_previous_state() {
    let discard = EventsDiscardMiddleware::new();
    let matcher = RecordingMatcher::default();
    let matchers: Vec<&dyn Matcher<'_, usize>> = vec![&matcher];
    let matcher_middleware = MatcherMiddleware::new(&matchers, &Options, &Options);
    let pipeline: Vec<&dyn Middleware> = vec![&discard, &matcher_middleware];

    run(&pipeline, key(1, Key::Other(0)));
    run(&pipeline, key(2, Key::Other(0)));
    assert_eq!(*matcher.previous_states.borrow(), vec![None, Some(1)]);

    let result = run(&pipeline, ended(2, 10));
    assert!(matches!(result.etype, EventType::FormInteractionEnded(_)));
    assert!(matches!(
        run(&pipeline, key(9, Key::Other(0))).etype,
        EventType::NOOP
    ));
    run(&pipeline, key(10, Key::Other(0)));
    assert_eq!(*matcher.previous_states.borrow(), vec![None, Some(1), None]);
}

#[test]
fn form_completion_clears_earlier_completed_undo_even_without_new_trigger_deletion() {
    let undo = UndoMiddleware::new(&Options);
    let pipeline: Vec<&dyn Middleware> = vec![&undo];
    run(
        &pipeline,
        Event::caused_by(
            1,
            EventType::TriggerCompensation(TriggerCompensationEvent {
                trigger: ":old".to_owned(),
                left_separator: None,
            }),
        ),
    );
    run(
        &pipeline,
        Event::caused_by(
            1,
            EventType::Rendered(RenderedEvent {
                match_id: 1,
                body: "old expansion".to_owned(),
                format: TextFormat::Plain,
            }),
        ),
    );

    // Search/hotkey forms do not replace the old record with TriggerCompensation.
    run(&pipeline, ended(2, 10));
    assert!(matches!(
        run(&pipeline, key(10, Key::Backspace)).etype,
        EventType::Keyboard(_)
    ));
}

#[test]
fn form_completion_bypasses_old_discard_range_and_cannot_move_cutoff_backwards() {
    let discard = EventsDiscardMiddleware::new();
    let pipeline: Vec<&dyn Middleware> = vec![&discard];
    run(
        &pipeline,
        Event::caused_by(
            1,
            EventType::DiscardBetween(DiscardBetweenEvent {
                start_id: 5,
                end_id: 20,
            }),
        ),
    );
    // The completion itself may refer to an ID that the search UI discarded.
    assert!(matches!(
        run(&pipeline, ended(10, 30)).etype,
        EventType::FormInteractionEnded(_)
    ));
    run(&pipeline, ended(10, 15));
    // Reset the lower bound as well: all earlier queued input is stale.
    for source_id in [1, 10, 20, 29] {
        assert!(matches!(
            run(&pipeline, key(source_id, Key::Other(0))).etype,
            EventType::NOOP
        ));
    }
    assert!(matches!(
        run(&pipeline, key(30, Key::Other(0))).etype,
        EventType::Keyboard(_)
    ));
}
