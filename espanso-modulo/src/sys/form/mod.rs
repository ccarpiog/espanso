/*
 * This file is part of modulo.
 *
 * Copyright (C) 2020-2021 Federico Terzi
 *
 * modulo is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * modulo is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with modulo.  If not, see <https://www.gnu.org/licenses/>.
 */

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_int;

// Form schema

pub mod types {
    #[derive(Debug)]
    pub struct Form {
        pub title: String,
        pub icon: Option<String>,
        pub fields: Vec<Field>,
        pub max_form_width: i32,
        pub max_form_height: i32,
    }

    #[derive(Debug)]
    pub struct Field {
        pub id: Option<String>,
        pub field_type: FieldType,
    }

    impl Default for Field {
        fn default() -> Self {
            Self {
                id: None,
                field_type: FieldType::Unknown,
            }
        }
    }

    #[derive(Debug)]
    pub enum FieldType {
        Unknown,
        Row(RowMetadata),
        Label(LabelMetadata),
        Text(TextMetadata),
        Choice(ChoiceMetadata),
    }

    #[derive(Debug)]
    pub struct RowMetadata {
        pub fields: Vec<Field>,
    }

    #[derive(Debug)]
    pub struct LabelMetadata {
        pub text: String,
    }

    #[derive(Debug)]
    pub struct TextMetadata {
        pub default_text: String,
        pub multiline: bool,
    }

    #[derive(Debug)]
    pub enum ChoiceType {
        Dropdown,
        List,
    }

    #[derive(Debug)]
    pub struct ChoiceMetadata {
        pub values: Vec<String>,
        pub choice_type: ChoiceType,
        pub default_value: String,
        pub separator: String,
    }
}

// Form interop

#[allow(dead_code)]
mod interop {
    use crate::sys;

    use super::super::interop::{
        ChoiceMetadata, ChoiceType_DROPDOWN, ChoiceType_LIST, FieldMetadata, FieldType,
        FieldType_CHOICE, FieldType_LABEL, FieldType_ROW, FieldType_TEXT, FormMetadata,
        Interoperable, LabelMetadata, RowMetadata, TextMetadata,
    };
    use super::types;
    use std::ffi::{c_void, CString};
    use std::os::raw::{c_char, c_int};
    use std::ptr::null;

    pub struct OwnedForm {
        title: CString,
        icon_path: CString,
        fields: Vec<OwnedField>,

        metadata: Vec<FieldMetadata>,
        interop: Box<FormMetadata>,
    }

    impl Interoperable for OwnedForm {
        fn as_ptr(&self) -> *const c_void {
            std::ptr::from_ref::<FormMetadata>(&(*self.interop)) as *const c_void
        }
    }

    impl From<types::Form> for OwnedForm {
        fn from(form: types::Form) -> Self {
            let title = CString::new(form.title).expect("unable to convert form title to CString");
            let fields: Vec<OwnedField> = form.fields.into_iter().map(Into::into).collect();

            let metadata: Vec<FieldMetadata> = fields
                .iter()
                .map(sys::form::interop::OwnedField::metadata)
                .collect();

            let icon_path = if let Some(icon_path) = form.icon.as_ref() {
                icon_path.clone()
            } else {
                String::new()
            };

            let icon_path =
                CString::new(icon_path).expect("unable to convert form icon to CString");

            let icon_path_ptr = if form.icon.is_some() {
                icon_path.as_ptr()
            } else {
                std::ptr::null()
            };

            let max_form_width = form.max_form_width;
            let max_form_height = form.max_form_height;

            let interop = Box::new(FormMetadata {
                windowTitle: title.as_ptr(),
                iconPath: icon_path_ptr,
                fields: metadata.as_ptr(),
                fieldSize: fields.len() as c_int,
                maxWindowWidth: max_form_width,
                maxWindowHeight: max_form_height,
            });

            Self {
                title,
                icon_path,
                fields,
                metadata,
                interop,
            }
        }
    }

    struct OwnedField {
        id: Option<CString>,
        field_type: FieldType,
        specific: Box<dyn Interoperable>,
    }

    impl From<types::Field> for OwnedField {
        fn from(field: types::Field) -> Self {
            let id = field
                .id
                .map(|id| CString::new(id).expect("unable to create cstring for field id"));

            let field_type = match field.field_type {
                types::FieldType::Row(_) => FieldType_ROW,
                types::FieldType::Label(_) => FieldType_LABEL,
                types::FieldType::Text(_) => FieldType_TEXT,
                types::FieldType::Choice(_) => FieldType_CHOICE,
                types::FieldType::Unknown => panic!("unknown field type"),
            };

            // TODO: clean up this match
            let specific: Box<dyn Interoperable> = match field.field_type {
                types::FieldType::Row(metadata) => {
                    let owned_metadata: OwnedRowMetadata = metadata.into();
                    Box::new(owned_metadata)
                }
                types::FieldType::Label(metadata) => {
                    let owned_metadata: OwnedLabelMetadata = metadata.into();
                    Box::new(owned_metadata)
                }
                types::FieldType::Text(metadata) => {
                    let owned_metadata: OwnedTextMetadata = metadata.into();
                    Box::new(owned_metadata)
                }
                types::FieldType::Choice(metadata) => {
                    let owned_metadata: OwnedChoiceMetadata = metadata.into();
                    Box::new(owned_metadata)
                }
                types::FieldType::Unknown => panic!("unknown field type"),
            };

            Self {
                id,
                field_type,
                specific,
            }
        }
    }

    impl OwnedField {
        pub fn metadata(&self) -> FieldMetadata {
            let id_ptr = if let Some(id) = self.id.as_ref() {
                id.as_ptr()
            } else {
                null()
            };

            FieldMetadata {
                id: id_ptr,
                fieldType: self.field_type,
                specific: self.specific.as_ptr(),
            }
        }
    }

    struct OwnedLabelMetadata {
        text: CString,
        interop: Box<LabelMetadata>,
    }

    impl Interoperable for OwnedLabelMetadata {
        fn as_ptr(&self) -> *const c_void {
            std::ptr::from_ref::<LabelMetadata>(&(*self.interop)) as *const c_void
        }
    }

    impl From<types::LabelMetadata> for OwnedLabelMetadata {
        fn from(label_metadata: types::LabelMetadata) -> Self {
            let text =
                CString::new(label_metadata.text).expect("unable to convert label text to CString");
            let interop = Box::new(LabelMetadata {
                text: text.as_ptr(),
            });
            Self { text, interop }
        }
    }

    struct OwnedTextMetadata {
        default_text: CString,
        interop: Box<TextMetadata>,
    }

    impl Interoperable for OwnedTextMetadata {
        fn as_ptr(&self) -> *const c_void {
            std::ptr::from_ref::<TextMetadata>(&(*self.interop)) as *const c_void
        }
    }

    impl From<types::TextMetadata> for OwnedTextMetadata {
        fn from(text_metadata: types::TextMetadata) -> Self {
            let default_text = CString::new(text_metadata.default_text)
                .expect("unable to convert default text to CString");
            let interop = Box::new(TextMetadata {
                defaultText: default_text.as_ptr(),
                multiline: i32::from(text_metadata.multiline),
            });
            Self {
                default_text,
                interop,
            }
        }
    }

    struct OwnedChoiceMetadata {
        values: Vec<CString>,
        values_ptr_array: Vec<*const c_char>,
        default_value: CString,
        separator: CString,
        interop: Box<ChoiceMetadata>,
    }

    impl Interoperable for OwnedChoiceMetadata {
        fn as_ptr(&self) -> *const c_void {
            std::ptr::from_ref::<ChoiceMetadata>(&(*self.interop)) as *const c_void
        }
    }

    impl From<types::ChoiceMetadata> for OwnedChoiceMetadata {
        fn from(metadata: types::ChoiceMetadata) -> Self {
            let values: Vec<CString> = metadata
                .values
                .into_iter()
                .map(|value| CString::new(value).expect("unable to convert choice value to string"))
                .collect();

            let values_ptr_array: Vec<*const c_char> =
                values.iter().map(|value| value.as_ptr()).collect();

            let choice_type = match metadata.choice_type {
                types::ChoiceType::Dropdown => ChoiceType_DROPDOWN,
                types::ChoiceType::List => ChoiceType_LIST,
            };

            let default_value = CString::new(metadata.default_value)
                .expect("unable to convert default value to CString");

            let separator =
                CString::new(metadata.separator).expect("unable to convert separator to CString");

            let interop = Box::new(ChoiceMetadata {
                values: values_ptr_array.as_ptr(),
                valueSize: values.len() as c_int,
                choiceType: choice_type,
                defaultValue: default_value.as_ptr(),
                separator: separator.as_ptr(),
            });
            Self {
                values,
                values_ptr_array,
                default_value,
                separator,
                interop,
            }
        }
    }

    struct OwnedRowMetadata {
        fields: Vec<OwnedField>,

        metadata: Vec<FieldMetadata>,
        interop: Box<RowMetadata>,
    }

    impl Interoperable for OwnedRowMetadata {
        fn as_ptr(&self) -> *const c_void {
            std::ptr::from_ref::<RowMetadata>(&(*self.interop)) as *const c_void
        }
    }

    impl From<types::RowMetadata> for OwnedRowMetadata {
        fn from(row_metadata: types::RowMetadata) -> Self {
            let fields: Vec<OwnedField> = row_metadata.fields.into_iter().map(Into::into).collect();

            let metadata: Vec<FieldMetadata> = fields
                .iter()
                .map(sys::form::interop::OwnedField::metadata)
                .collect();

            let interop = Box::new(RowMetadata {
                fields: metadata.as_ptr(),
                fieldSize: metadata.len() as c_int,
            });

            Self {
                fields,
                metadata,
                interop,
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FormAction {
    Cancel,
    Copy,
    Insert,
}

#[derive(Debug, serde::Serialize)]
pub struct FormResult {
    pub action: FormAction,
    pub values: HashMap<String, String>,
}

pub fn show(form: types::Form) -> HashMap<String, String> {
    let result = show_inner(form, None, false, false);
    if result.action == FormAction::Cancel {
        HashMap::new()
    } else {
        result.values
    }
}

pub fn show_recoverable(
    form: types::Form,
    draft_path: Option<&std::path::Path>,
    supports_insert: bool,
) -> FormResult {
    show_inner(form, draft_path, true, supports_insert)
}

fn show_inner(
    form: types::Form,
    draft_path: Option<&std::path::Path>,
    recovery_mode: bool,
    supports_insert: bool,
) -> FormResult {
    use super::interop::{
        interop_show_form, FormMetadata, FormRecoveryMetadata, Interoperable, ValuePair,
    };
    use crate::form::draft::{self, Draft};
    use std::{ffi::CString, os::raw::c_void, path::Path};

    let (mut saved, error) = if recovery_mode {
        match draft_path.map(draft::load).transpose() {
            Ok(saved) => (saved.flatten(), String::new()),
            Err(error) => (None, format!("Could not load saved entries: {error}")),
        }
    } else {
        (None, String::new())
    };
    if let Some(saved) = &mut saved {
        saved.retain_available_choices(&form.fields);
    }
    let owned_form: interop::OwnedForm = form.into();
    let metadata: *const FormMetadata = owned_form.as_ptr() as *const FormMetadata;
    let error = CString::new(error.replace('\0', "")).unwrap();
    let saved_strings: Vec<(CString, Option<CString>)> = saved
        .as_ref()
        .map(|draft| {
            draft
                .fields
                .iter()
                .flat_map(|(id, values)| {
                    let values: Vec<Option<&str>> = if values.is_empty() {
                        vec![None]
                    } else {
                        values.iter().map(|v| Some(v.as_str())).collect()
                    };
                    values.into_iter().map(move |value| {
                        (
                            CString::new(id.as_str()).unwrap(),
                            value.map(|value| CString::new(value).unwrap()),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let saved_values: Vec<ValuePair> = saved_strings
        .iter()
        .map(|(id, value)| ValuePair {
            id: id.as_ptr(),
            value: value
                .as_ref()
                .map_or(std::ptr::null(), |value| value.as_ptr()),
        })
        .collect();
    let recovery = FormRecoveryMetadata {
        enabled: i32::from(recovery_mode),
        supportsInsert: i32::from(supports_insert),
        hasDraft: i32::from(saved.is_some()),
        savedValues: saved_values.as_ptr(),
        savedValueSize: saved_values.len() as c_int,
        draftError: error.as_ptr(),
    };
    struct State<'a> {
        result: FormResult,
        path: Option<&'a Path>,
    }
    let mut state = State {
        result: FormResult {
            action: FormAction::Cancel,
            values: HashMap::new(),
        },
        path: draft_path,
    };

    extern "C" fn callback(
        values: *const ValuePair,
        size: c_int,
        action: c_int,
        data: *mut c_void,
    ) {
        let state = unsafe { &mut *data.cast::<State<'_>>() };
        state.result.action = match action {
            1 => FormAction::Copy,
            2 => FormAction::Insert,
            _ => FormAction::Cancel,
        };
        if size <= 0 {
            return;
        }
        let values = unsafe { std::slice::from_raw_parts(values, size as usize) };
        for pair in values {
            let id = unsafe { CStr::from_ptr(pair.id) }
                .to_string_lossy()
                .into_owned();
            let value = unsafe { CStr::from_ptr(pair.value) }
                .to_string_lossy()
                .into_owned();
            state.result.values.insert(id, value);
        }
    }

    extern "C" fn save_draft(values: *const ValuePair, size: c_int, data: *mut c_void) -> c_int {
        // Never unwind across the native callback boundary.
        let result = std::panic::catch_unwind(|| -> anyhow::Result<()> {
            let state = unsafe { &*data.cast::<State<'_>>() };
            let path = state
                .path
                .ok_or_else(|| anyhow::anyhow!("Draft recovery is unavailable"))?;
            let mut draft = Draft::default();
            if size > 0 {
                let values = unsafe { std::slice::from_raw_parts(values, size as usize) };
                for pair in values {
                    let id = unsafe { CStr::from_ptr(pair.id) }
                        .to_string_lossy()
                        .into_owned();
                    let entry = draft.fields.entry(id).or_default();
                    if !pair.value.is_null() {
                        entry.push(
                            unsafe { CStr::from_ptr(pair.value) }
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
            }
            draft::save(path, &draft)
        });
        i32::from(matches!(result, Ok(Ok(()))))
    }

    unsafe {
        interop_show_form(
            metadata,
            &recovery,
            callback,
            save_draft,
            std::ptr::from_mut(&mut state).cast(),
        );
    }
    state.result
}
