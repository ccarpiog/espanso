/*
 * This file is part of espanso.
 *
 * Copyright (C) 2019-2021 Federico Terzi
 *
 * espanso is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * espanso is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with espanso.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::{cell::RefCell, collections::HashMap, sync::Arc};

pub mod extension;

use espanso_config::{
    config::Config,
    matches::{store::MatchSet, Match, MatchCause, MatchEffect, UpperCasingStyle},
};
use espanso_render::{CasingStyle, Context, RenderOptions, Template, Value, Variable};

use crate::gui::recovery::{fingerprint, FormRecovery, LastForm};
use espanso_engine::process::{Renderer, RendererError};

pub trait MatchProvider<'a> {
    fn matches(&self) -> Vec<&'a Match>;
    fn get(&self, id: i32) -> Option<&'a Match>;
}

pub trait ConfigProvider<'a> {
    fn configs(&'_ self) -> Vec<(Arc<dyn Config>, MatchSet<'_>)>;
    fn active(&'_ self) -> (Arc<dyn Config>, MatchSet<'_>);
}

pub struct RendererAdapter<'a> {
    recovery: &'a FormRecovery,
    renderer: &'a dyn espanso_render::Renderer,
    match_provider: &'a dyn MatchProvider<'a>,
    config_provider: &'a dyn ConfigProvider<'a>,

    template_map: HashMap<i32, Option<Template>>,
    global_vars_map: HashMap<i32, Variable>,

    context_cache: RefCell<HashMap<i32, Context<'a>>>,
}

impl<'a> RendererAdapter<'a> {
    pub fn new(
        match_provider: &'a dyn MatchProvider<'a>,
        config_provider: &'a dyn ConfigProvider<'a>,
        renderer: &'a dyn espanso_render::Renderer,
        recovery: &'a FormRecovery,
    ) -> Self {
        let template_map = generate_template_map(match_provider);
        let global_vars_map = generate_global_vars_map(config_provider);

        Self {
            recovery,
            renderer,
            config_provider,
            match_provider,
            template_map,
            global_vars_map,
            context_cache: RefCell::new(HashMap::new()),
        }
    }
}

fn generate_template_map(match_provider: &dyn MatchProvider) -> HashMap<i32, Option<Template>> {
    let mut template_map = HashMap::new();
    for m in match_provider.matches() {
        let entry = convert_to_template(m);
        template_map.insert(m.id, entry);
    }
    template_map
}

fn generate_global_vars_map(config_provider: &dyn ConfigProvider) -> HashMap<i32, Variable> {
    let mut global_vars_map = HashMap::new();

    for (_, match_set) in config_provider.configs() {
        for var in &match_set.global_vars {
            global_vars_map
                .entry(var.id)
                .or_insert_with(|| convert_var((*var).clone()));
        }
    }

    global_vars_map
}

fn generate_context<'a>(
    match_set: &MatchSet,
    template_map: &'a HashMap<i32, Option<Template>>,
    global_vars_map: &'a HashMap<i32, Variable>,
) -> Context<'a> {
    let mut templates = Vec::new();
    let mut global_vars = Vec::new();

    for m in &match_set.matches {
        if let Some(Some(template)) = template_map.get(&m.id) {
            templates.push(template);
        }
    }

    for var in &match_set.global_vars {
        if let Some(var) = global_vars_map.get(&var.id) {
            global_vars.push(var);
        }
    }

    Context {
        global_vars,
        templates,
    }
}

// TODO: move conversion methods to new file?

fn convert_to_template(m: &Match) -> Option<Template> {
    if let MatchEffect::Text(text_effect) = &m.effect {
        let ids = if let MatchCause::Trigger(cause) = &m.cause {
            cause.triggers.clone()
        } else {
            Vec::new()
        };

        Some(Template {
            ids,
            body: text_effect.replace.clone(),
            vars: convert_vars(text_effect.vars.clone()),
        })
    } else {
        None
    }
}

fn convert_vars(vars: Vec<espanso_config::matches::Variable>) -> Vec<espanso_render::Variable> {
    vars.into_iter().map(convert_var).collect()
}

fn convert_var(var: espanso_config::matches::Variable) -> espanso_render::Variable {
    Variable {
        name: var.name,
        var_type: var.var_type,
        params: convert_params(var.params),
        inject_vars: var.inject_vars,
        depends_on: var.depends_on,
    }
}

fn convert_params(params: espanso_config::matches::Params) -> espanso_render::Params {
    let mut new_params = espanso_render::Params::new();
    for (key, value) in params {
        new_params.insert(key, convert_value(value));
    }
    new_params
}

fn convert_value(value: espanso_config::matches::Value) -> espanso_render::Value {
    match value {
        espanso_config::matches::Value::Null => espanso_render::Value::Null,
        espanso_config::matches::Value::Bool(v) => espanso_render::Value::Bool(v),
        espanso_config::matches::Value::Number(n) => match n {
            espanso_config::matches::Number::Integer(i) => {
                espanso_render::Value::Number(espanso_render::Number::Integer(i))
            }
            espanso_config::matches::Number::Float(f) => {
                espanso_render::Value::Number(espanso_render::Number::Float(f.into_inner()))
            }
        },
        espanso_config::matches::Value::String(s) => espanso_render::Value::String(s),
        espanso_config::matches::Value::Array(v) => {
            espanso_render::Value::Array(v.into_iter().map(convert_value).collect())
        }
        espanso_config::matches::Value::Object(params) => {
            espanso_render::Value::Object(convert_params(params))
        }
    }
}

impl<'a> Renderer<'a> for RendererAdapter<'a> {
    fn retain_interactive_result(
        &self,
        body: &str,
        format: &espanso_engine::event::internal::TextFormat,
    ) -> anyhow::Result<bool> {
        if !self.recovery.used() {
            return Ok(false);
        }
        let (result, cursor_moves) =
            crate::cli::worker::engine::form_recovery::prepare_result(body, format);
        if let Err(error) = self.recovery.finish(result, cursor_moves) {
            // The completed result is retained in memory even if persistence
            // fails. RecoveryProcessor copies it and reports the save failure;
            // emitting RenderingError here would add a contradictory message.
            log::error!("could not persist form result: {error}");
        }
        Ok(true)
    }

    fn render(
        &'a self,
        match_id: i32,
        trigger: Option<&str>,
        trigger_vars: HashMap<String, String>,
    ) -> anyhow::Result<String> {
        if let Some(Some(template)) = self.template_map.get(&match_id) {
            let (config, match_set) = if let Some(expected) = self.recovery.reopen_context() {
                self.config_provider
                    .configs()
                    .into_iter()
                    .find(|(config, matches)| context_key(&**config, matches) == expected)
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "saved form configuration changed; copy the saved result instead"
                        )
                    })?
            } else {
                self.config_provider.active()
            };
            if let Some(m) = self.match_provider.get(match_id) {
                self.recovery.prepare(LastForm {
                    fingerprint: fingerprint(m),
                    context_key: context_key(&*config, &match_set),
                    trigger: trigger.map(str::to_owned),
                    args: trigger_vars.clone(),
                });
            }

            let mut context_cache = self.context_cache.borrow_mut();
            let context = context_cache.entry(config.id()).or_insert_with(|| {
                generate_context(&match_set, &self.template_map, &self.global_vars_map)
            });

            let raw_match = self.match_provider.get(match_id);
            let propagate_case = raw_match.is_some_and(is_propagate_case);
            let preferred_uppercasing_style = raw_match.and_then(extract_uppercasing_style);

            let options = RenderOptions {
                casing_style: if !propagate_case {
                    CasingStyle::None
                } else if let Some(trigger) = trigger {
                    calculate_casing_style(trigger, preferred_uppercasing_style)
                } else {
                    CasingStyle::None
                },
            };

            // If some trigger vars are specified, augment the template with them
            let augmented_template = if trigger_vars.is_empty() {
                None
            } else {
                let mut augmented = template.clone();
                for (name, value) in trigger_vars {
                    let mut params = espanso_render::Params::new();
                    params.insert("echo".to_string(), Value::String(value));
                    augmented.vars.insert(
                        0,
                        Variable {
                            name,
                            var_type: "echo".to_string(),
                            params,
                            inject_vars: false,
                            ..Default::default()
                        },
                    );
                }
                Some(augmented)
            };

            let template = if let Some(augmented) = augmented_template.as_ref() {
                augmented
            } else {
                template
            };

            capture_if_interactive(template, context, || self.recovery.capture_target());

            match self.renderer.render(template, context, &options) {
                espanso_render::RenderResult::Success(body) => Ok(body),
                espanso_render::RenderResult::Aborted => Err(RendererError::Aborted.into()),
                espanso_render::RenderResult::Error(err) => {
                    Err(RendererError::RenderingError(err).into())
                }
            }
        } else {
            Err(RendererError::NotFound.into())
        }
    }
}

fn capture_if_interactive(template: &Template, context: &Context, capture: impl FnOnce()) {
    if espanso_render::may_open_ui(template, context) {
        capture();
    }
}

fn context_key(config: &dyn Config, match_set: &MatchSet) -> String {
    let globals: Vec<_> = match_set
        .global_vars
        .iter()
        .map(|var| {
            let mut var = (*var).clone();
            var.id = 0;
            var
        })
        .collect();
    crate::gui::recovery::digest(&format!("v1:{:?}:{globals:?}", config.match_paths()))
}

fn extract_uppercasing_style(m: &Match) -> Option<UpperCasingStyle> {
    if let MatchCause::Trigger(cause) = &m.cause {
        Some(cause.uppercase_style.clone())
    } else {
        None
    }
}

fn is_propagate_case(m: &Match) -> bool {
    if let MatchCause::Trigger(cause) = &m.cause {
        cause.propagate_case
    } else {
        false
    }
}

fn calculate_casing_style(
    trigger: &str,
    uppercasing_style: Option<UpperCasingStyle>,
) -> CasingStyle {
    let mut first_alphabetic = None;
    let mut second_alphabetic = None;

    for c in trigger.chars() {
        if c.is_alphabetic() {
            if first_alphabetic.is_none() {
                first_alphabetic = Some(c);
            } else if second_alphabetic.is_none() {
                second_alphabetic = Some(c);
            } else {
                break;
            }
        }
    }

    if let Some(first) = first_alphabetic {
        if let Some(second) = second_alphabetic {
            if first.is_uppercase() {
                if second.is_uppercase() {
                    CasingStyle::Uppercase
                } else {
                    match uppercasing_style {
                        Some(UpperCasingStyle::CapitalizeWords) => CasingStyle::CapitalizeWords,
                        _ => CasingStyle::Capitalize,
                    }
                }
            } else {
                CasingStyle::None
            }
        } else if first.is_uppercase() {
            match uppercasing_style {
                Some(UpperCasingStyle::Capitalize) => CasingStyle::Capitalize,
                Some(UpperCasingStyle::CapitalizeWords) => CasingStyle::CapitalizeWords,
                _ => CasingStyle::Uppercase,
            }
        } else {
            CasingStyle::None
        }
    } else {
        CasingStyle::None
    }
}

#[cfg(test)]
mod recovery_tests {
    use super::{capture_if_interactive, ConfigProvider, MatchProvider, RendererAdapter};
    use crate::gui::recovery::{FormRecovery, LastForm};
    use espanso_config::{
        config::Config,
        matches::{store::MatchSet, Match},
    };
    use espanso_engine::{event::internal::TextFormat, process::Renderer};
    use espanso_render::{
        Context, Extension, ExtensionOutput, ExtensionResult, Params, RenderOptions, RenderResult,
        Scope, Template, Variable,
    };
    use std::{cell::Cell, collections::HashMap, sync::Arc};

    #[test]
    fn capture_is_skipped_for_ordinary_snippets_and_precedes_scripts_before_forms() {
        let captures = Cell::new(0);
        let form = Variable {
            name: "form".into(),
            var_type: "form".into(),
            ..Variable::default()
        };
        let context = Context {
            global_vars: vec![&form],
            ..Context::default()
        };
        let date = Template {
            body: "{{date}}".into(),
            vars: vec![Variable {
                name: "date".into(),
                var_type: "date".into(),
                ..Variable::default()
            }],
            ..Template::default()
        };
        let plain = Template {
            body: "hello".into(),
            ..Template::default()
        };
        for template in [&plain, &date] {
            capture_if_interactive(template, &context, || captures.set(captures.get() + 1));
        }
        assert_eq!(captures.get(), 0);

        struct CheckedExtension<'a> {
            name: &'static str,
            captures: &'a Cell<usize>,
            calls: &'a Cell<usize>,
        }
        impl Extension for CheckedExtension<'_> {
            fn name(&self) -> &str {
                self.name
            }
            fn calculate(&self, _: &Context, _: &Scope, _: &Params) -> ExtensionResult {
                assert_eq!(
                    self.captures.get(),
                    1,
                    "capture must precede even the first shell extension"
                );
                self.calls.set(self.calls.get() + 1);
                ExtensionResult::Success(ExtensionOutput::Single(self.name.into()))
            }
        }
        let calls = Cell::new(0);
        let shell = CheckedExtension {
            name: "shell",
            captures: &captures,
            calls: &calls,
        };
        let form_extension = CheckedExtension {
            name: "form",
            captures: &captures,
            calls: &calls,
        };
        let renderer = espanso_render::create(vec![&shell, &form_extension]);
        let template = Template {
            body: "{{form}}".into(),
            vars: vec![
                Variable {
                    name: "script".into(),
                    var_type: "shell".into(),
                    ..Variable::default()
                },
                form.clone(),
            ],
            ..Template::default()
        };
        capture_if_interactive(&template, &context, || captures.set(captures.get() + 1));
        assert!(matches!(
            espanso_render::Renderer::render(
                &renderer,
                &template,
                &context,
                &RenderOptions::default()
            ),
            RenderResult::Success(_)
        ));
        assert_eq!(calls.get(), 2);
    }

    struct EmptyProviders;
    impl<'a> MatchProvider<'a> for EmptyProviders {
        fn matches(&self) -> Vec<&'a Match> {
            Vec::new()
        }
        fn get(&self, _: i32) -> Option<&'a Match> {
            None
        }
    }
    impl ConfigProvider<'_> for EmptyProviders {
        fn configs(&self) -> Vec<(Arc<dyn Config>, MatchSet<'_>)> {
            Vec::new()
        }
        fn active(&self) -> (Arc<dyn Config>, MatchSet<'_>) {
            panic!("no configuration needed for retention")
        }
    }

    #[test]
    fn failed_result_save_retains_completion_without_a_rendering_error() {
        let directory = tempfile::tempdir().unwrap();
        let recovery = FormRecovery::new(directory.path());
        recovery.prevent_target_capture_for_test();
        recovery.prepare(LastForm {
            fingerprint: "match".into(),
            context_key: "context".into(),
            trigger: Some(":form".into()),
            args: HashMap::new(),
        });
        recovery.open_form("[[name]]").unwrap();
        // A directory at the destination deterministically prevents atomic replacement.
        std::fs::create_dir(directory.path().join("form-recovery/last-result.json")).unwrap();
        let renderer = espanso_render::create(Vec::new());
        let providers = EmptyProviders;
        let adapter = RendererAdapter::new(&providers, &providers, &renderer, &recovery);

        assert!(adapter
            .retain_interactive_result("retained result", &TextFormat::Plain)
            .unwrap());
        assert!(recovery.persistence_failed());
        assert_eq!(recovery.last_result().unwrap().text, "retained result");
        let completed = recovery
            .take_completed()
            .expect("completion must reach the recovery processor");
        assert_eq!(completed.result.text, "retained result");
        assert!(!completed.insert);
    }
}
