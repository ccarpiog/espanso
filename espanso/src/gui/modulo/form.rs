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

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::convert::TryInto;

use crate::gui::recovery::{FormAction, FormRecovery, FormResponse};
use crate::gui::{FormField, FormUI};

use super::manager::ModuloManager;

pub trait ModuloFormUIOptionProvider {
    fn get_post_form_delay(&self) -> usize;
    fn get_max_form_width(&self) -> usize;
    fn get_max_form_height(&self) -> usize;
}

pub struct ModuloFormUI<'a> {
    manager: &'a ModuloManager,
    option_provider: &'a dyn ModuloFormUIOptionProvider,
    recovery: &'a FormRecovery,
}

impl<'a> ModuloFormUI<'a> {
    pub fn new(
        manager: &'a ModuloManager,
        option_provider: &'a dyn ModuloFormUIOptionProvider,
        recovery: &'a FormRecovery,
    ) -> Self {
        Self {
            manager,
            option_provider,
            recovery,
        }
    }
}

impl FormUI for ModuloFormUI<'_> {
    fn show(
        &self,
        layout: &str,
        fields: &HashMap<String, FormField>,
    ) -> anyhow::Result<Option<HashMap<String, String>>> {
        let fields = convert_fields_into_object(fields);
        let schema = recovery_schema(layout, &fields)?;
        let draft_path = self.recovery.open_form(&schema)?;
        let modulo_form_config = ModuloFormConfig {
            title: "espanso",
            layout,
            fields,
            recovery_mode: true,
            supports_insert: cfg!(target_os = "macos"),
            draft_path,
            max_form_width: self.option_provider.get_max_form_width(),
            max_form_height: self.option_provider.get_max_form_height(),
        };

        let json_config = serde_json::to_string(&modulo_form_config)?;
        let output = self
            .manager
            .invoke(&["form", "-j", "-i", "-"], &json_config)?;
        let json: Result<FormResponse, _> = serde_json::from_str(&output);
        let result = match json {
            Ok(json) => {
                self.recovery.select(json.action);
                if json.action == FormAction::Cancel {
                    Ok(None)
                } else {
                    Ok(Some(json.values))
                }
            }
            Err(error) => Err(error.into()),
        };

        let post_form_delay = self.option_provider.get_post_form_delay();
        if post_form_delay > 0 {
            std::thread::sleep(std::time::Duration::from_millis(
                post_form_delay.try_into().unwrap(),
            ));
        }

        result
    }
}

#[derive(Debug, Serialize)]
struct ModuloFormConfig<'a> {
    recovery_mode: bool,
    supports_insert: bool,
    draft_path: std::path::PathBuf,
    title: &'a str,
    layout: &'a str,
    fields: Map<String, Value>,
    max_form_width: usize,
    max_form_height: usize,
}

/// Only the actual input identities and types define draft compatibility.
/// Labels, defaults, options and separators can be supplied by dynamic vars;
/// changing those must not make a saved draft unreachable. The outer match
/// fingerprint already separates changes to the raw snippet configuration.
#[cfg(feature = "modulo")]
fn recovery_schema(layout: &str, fields: &Map<String, Value>) -> anyhow::Result<String> {
    use espanso_modulo::form::parser::layout::{parse_layout, Token};
    let mut schema = std::collections::BTreeMap::new();
    for token in parse_layout(layout).into_iter().flatten() {
        if let Token::Field(name) = token {
            let field_type = fields
                .get(&name)
                .and_then(|field| field.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("text");
            schema.insert(name, field_type);
        }
    }
    Ok(serde_json::to_string(&schema)?)
}

#[cfg(not(feature = "modulo"))]
fn recovery_schema(_: &str, _: &Map<String, Value>) -> anyhow::Result<String> {
    anyhow::bail!("this build does not include form support")
}

fn convert_fields_into_object(fields: &HashMap<String, FormField>) -> Map<String, Value> {
    let mut obj = Map::new();
    for (name, field) in fields {
        let value = match field {
            FormField::Text { default, multiline } => json!({
              "type": "text",
              "default": default,
              "multiline": multiline,
            }),
            FormField::Choice { default, values } => json!({
              "type": "choice",
              "default": default,
              "values": values,
            }),
            FormField::List {
                default,
                values,
                separator,
            } => json!({
              "type": "list",
              "default": default,
              "values": values,
              "separator": separator,
            }),
        };
        obj.insert(name.clone(), value);
    }
    obj
}

#[cfg(all(test, feature = "modulo"))]
mod tests {
    use super::*;

    #[test]
    fn dynamic_layout_defaults_and_choices_keep_the_same_draft() {
        let yesterday = json!({
            "choice": {"type": "choice", "values": ["old", "shared"], "default": "old"},
            "items": {"type": "list", "values": ["one, two", "shared"], "separator": ", "}
        });
        let today = json!({
            "choice": {"type": "choice", "values": ["shared", "new"], "default": "new"},
            "items": {"type": "list", "values": ["shared", "third"], "separator": " | "}
        });
        let before = recovery_schema(
            "2026-11-09: [[choice]]\n[[items]] [[implicit]]",
            yesterday.as_object().unwrap(),
        )
        .unwrap();
        let after = recovery_schema(
            "2026-11-10: [[items]]\n[[choice]] [[implicit]]",
            today.as_object().unwrap(),
        )
        .unwrap();
        assert_eq!(before, after);
        assert!(before.contains("\"implicit\":\"text\""));
    }

    #[test]
    fn changed_input_id_or_type_uses_a_different_draft() {
        let fields = json!({"answer": {"type": "choice"}});
        let text_fields = json!({"answer": {"type": "text"}});
        let choice = recovery_schema("[[answer]]", fields.as_object().unwrap()).unwrap();
        assert_ne!(
            choice,
            recovery_schema("[[answer]]", text_fields.as_object().unwrap()).unwrap()
        );
        assert_ne!(
            choice,
            recovery_schema("[[renamed]]", fields.as_object().unwrap()).unwrap()
        );
        assert_eq!(
            choice,
            recovery_schema("{{answer}}", fields.as_object().unwrap()).unwrap()
        );
    }
}
