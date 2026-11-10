//! Form results are retained before any editing event reaches the dispatcher.
#[cfg(test)]
#[path = "form_recovery_tests.rs"]
mod transaction_tests;

use super::{
    dispatch::executor::clipboard_injector::ClipboardParamsProvider,
    process::middleware::render::MatchProvider,
};
use crate::gui::recovery::{fingerprint, FormRecovery, SavedResult};
use anyhow::Result;
use espanso_clipboard::{Clipboard, ClipboardOperationOptions};
use espanso_config::matches::MatchEffect;
use espanso_engine::{
    event::{
        input::Key,
        internal::{FormInteractionEndedEvent, RenderingRequestedEvent, TextFormat},
        Event, EventType,
    },
    process::{EventSequenceProvider, Processor},
};
use espanso_ui::UIRemote;
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;

pub struct RecoveryProcessor<'a> {
    pub inner: &'a mut dyn Processor,
    pub recovery: &'a FormRecovery,
    pub clipboard: &'a dyn Clipboard,
    pub params: &'a dyn ClipboardParamsProvider,
    pub sequence: &'a dyn EventSequenceProvider,
    pub matches: &'a dyn MatchProvider<'a>,
    pub ui: &'a dyn UIRemote,
}

impl RecoveryProcessor<'_> {
    fn copy(&self, result: &SavedResult) -> Result<()> {
        let options = ClipboardOperationOptions {
            use_xclip_backend: self.params.get().x11_use_xclip_backend,
        };
        if let Some(html) = &result.html {
            self.clipboard.set_html(html, Some(&result.text), &options)
        } else {
            self.clipboard.set_text(&result.text, &options)
        }
    }

    fn reopen(&self, source_id: u32) -> Result<Event> {
        let last = self.recovery.last_form()?;
        self.recovery.set_reopen_context(last.context_key.clone());
        let matching: Vec<_> = self
            .matches
            .matches()
            .into_iter()
            .filter(|m| fingerprint(m) == last.fingerprint)
            .collect();
        let [m] = matching.as_slice() else {
            anyhow::bail!("the saved snippet was changed, removed, or is ambiguous");
        };
        let MatchEffect::Text(effect) = &m.effect else {
            anyhow::bail!("saved snippet is not text");
        };
        let format = match effect.format {
            espanso_config::matches::TextFormat::Plain => TextFormat::Plain,
            espanso_config::matches::TextFormat::Markdown => TextFormat::Markdown,
            espanso_config::matches::TextFormat::Html => TextFormat::Html,
        };
        // This directly requests rendering: the old trigger is only casing context,
        // never a request to delete characters at the new destination.
        Ok(Event::caused_by(
            source_id,
            EventType::RenderingRequested(RenderingRequestedEvent {
                match_id: m.id,
                trigger: last.trigger,
                trigger_args: last.args,
                left_separator: None,
                right_separator: None,
                format,
            }),
        ))
    }

    fn complete(&self, events: &[Event]) {
        let Some(completed) = self.recovery.take_completed() else {
            return;
        };
        if let Err(err) = self.copy(&completed.result) {
            log::error!("could not copy form result: {err}");
            self.ui.show_notification("Could not copy the result. Use 'Copy last form result' to retry; your entries are saved.");
            return;
        }
        if self.recovery.persistence_failed() {
            self.ui.show_notification(
                "Result copied, but could not save it to disk. Paste it before restarting Espanso.",
            );
            return;
        }
        if !completed.insert {
            self.ui
                .show_notification("Form result copied. Select the destination and paste.");
            return;
        }
        let params = self.params.get();
        let standard_paste = params.paste_shortcut.as_deref().is_none_or(|shortcut| {
            matches!(shortcut.to_ascii_uppercase().as_str(), "CMD+V" | "META+V")
        });
        if !standard_paste || !self.recovery.restore_target() {
            self.ui.show_notification(
                "Could not safely restore the destination. Result copied; paste it manually.",
            );
            return;
        }
        // The ONLY queued key events allowed here are the original trigger's
        // backspaces. No queued effect is sent to the normal dispatcher.
        let Some(backspaces) = deletion_count(events) else {
            self.ui.show_notification(
                "Result copied. Paste manually to avoid editing the wrong text.",
            );
            return;
        };
        if !self.recovery.capture_clipboard(&completed.result) {
            self.ui.show_notification(
                "The clipboard changed. Use 'Copy last form result' and paste manually.",
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(params.pre_paste_delay as u64));
        // Native delivery checks the destination again after this delay and before
        // every key, synchronously. It never queues an unguarded global paste.
        if !self.recovery.paste_from_clipboard(
            backspaces,
            completed.cursor_moves,
            params.paste_shortcut_event_delay.min(1000) as u32,
        ) {
            self.ui.show_notification("Insertion stopped because the destination or clipboard changed. Use 'Copy last form result' to recover it.");
        }
    }
}

impl Processor for RecoveryProcessor<'_> {
    fn process(&mut self, event: Event) -> Vec<Event> {
        self.recovery.reset();
        let source_id = event.source_id;
        let mut events = self.inner.process(event);
        // Builtins can arrive from the search window or a hotkey.
        let reopen = events
            .iter()
            .any(|event| matches!(event.etype, EventType::ReopenLastForm));
        let copy_last = events
            .iter()
            .any(|event| matches!(event.etype, EventType::CopyLastFormResult));
        if reopen {
            match self.reopen(source_id) {
                Ok(event) => {
                    events.extend(self.inner.process(event));
                }
                Err(err) => {
                    log::warn!("could not reopen last form: {err}");
                    self.ui.show_notification("Cannot reopen the saved form. Its snippet may have changed or been removed.");
                }
            }
        }
        if copy_last {
            match self
                .recovery
                .last_result()
                .and_then(|result| self.copy(&result))
            {
                Ok(()) => self.ui.show_notification("Last form result copied."),
                Err(err) => {
                    log::warn!("could not copy last result: {err}");
                    self.ui
                        .show_notification("No saved form result could be copied.");
                }
            }
        }
        if self.recovery.used() || reopen || copy_last {
            self.complete(&events);
            // Flush all input accumulated while the UI was open, even after
            // cancellation, rendering failure, or a clipboard error.
            self.inner.process(Event::caused_by(
                source_id,
                EventType::FormInteractionEnded(FormInteractionEndedEvent {
                    minimum_source_id: self.sequence.get_next_id(),
                }),
            ));
            events.retain(|event| !is_editing_effect(&event.etype));
        }
        events
    }
}

fn is_editing_effect(event: &EventType) -> bool {
    matches!(
        event,
        EventType::KeySequenceInject(_)
            | EventType::TextInject(_)
            | EventType::HtmlInject(_)
            | EventType::MarkdownInject(_)
            | EventType::ImageInject(_)
    )
}

fn deletion_count(events: &[Event]) -> Option<usize> {
    let mut count = 0usize;
    for event in events {
        if let EventType::KeySequenceInject(keys) = &event.etype {
            if !keys.keys.iter().all(|key| *key == Key::Backspace) {
                return None;
            }
            count = count.checked_add(keys.keys.len())?;
        }
    }
    Some(count)
}

pub fn prepare_result(body: &str, format: &TextFormat) -> (SavedResult, usize) {
    let cursor_moves = body.find("$|$").map_or(0, |index| {
        body[index + 3..].replace("$|$", "").graphemes(true).count()
    });
    let text = body.replace("$|$", "");
    let html = match format {
        TextFormat::Plain => None,
        TextFormat::Html => Some(text.clone()),
        TextFormat::Markdown => {
            let mut html = String::new();
            pulldown_cmark::html::push_html(&mut html, pulldown_cmark::Parser::new(&text));
            let html = html.trim();
            Some(if html.matches("<p>").count() <= 1 {
                html.trim_start_matches("<p>")
                    .trim_end_matches("</p>")
                    .to_owned()
            } else {
                html.to_owned()
            })
        }
    };
    if let Some(html) = html {
        let text = html2text::from_read_with_decorator(
            html.as_bytes(),
            1_000_000,
            html2text::render::text_renderer::TrivialDecorator::new(),
        );
        // Rich text cursor positions cannot be inferred from markup byte offsets.
        (
            SavedResult {
                text,
                html: Some(html),
            },
            0,
        )
    } else {
        (SavedResult { text, html: None }, cursor_moves)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use espanso_engine::event::effect::{KeySequenceInjectRequest, TextInjectRequest};

    #[test]
    fn copy_output_removes_cursor_markers_and_keeps_unicode() {
        let (result, moves) = prepare_result("a$|$é👨‍👩‍👧\n", &TextFormat::Plain);
        assert_eq!(result.text, "aé👨‍👩‍👧\n");
        assert_eq!(moves, 3);
    }

    #[test]
    fn rich_copy_has_html_and_plain_fallback() {
        let (result, moves) = prepare_result("**Hello**", &TextFormat::Markdown);
        assert_eq!(result.html.as_deref(), Some("<strong>Hello</strong>"));
        assert!(result.text.contains("Hello"));
        assert_eq!(moves, 0);
    }

    #[test]
    fn deletion_rejects_non_backspace_sequences_and_filter_blocks_errors() {
        let event = Event::caused_by(
            1,
            EventType::KeySequenceInject(KeySequenceInjectRequest {
                keys: vec![Key::ArrowLeft],
            }),
        );
        assert!(deletion_count(&[event]).is_none());
        assert!(is_editing_effect(&EventType::TextInject(
            TextInjectRequest::default()
        )));
        assert_eq!(deletion_count(&[]), Some(0));
    }
}
