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

#define _UNICODE

#include "../common/common.h"
#include "../interop/interop.h"

#include <memory>
#include <string>
#include <wx/timer.h>
#include <wx/msgdlg.h>
#include <unordered_map>
#include <vector>

// https://docs.wxwidgets.org/stable/classwx_frame.html
const long DEFAULT_STYLE = wxSTAY_ON_TOP | wxCLOSE_BOX | wxCAPTION;

const int PADDING = 5;
const int MULTILINE_MIN_HEIGHT = 100;
const int MULTILINE_MIN_WIDTH = 100;

FormMetadata *formMetadata = nullptr;
std::vector<ValuePair> values;
const FormRecoveryMetadata *recoveryMetadata = nullptr;
int (*saveDraftCallback)(ValuePair *, int, void *) = nullptr;
void *formCallbackData = nullptr;
int formAction = 0; // cancel, copy, insert


// Field Wrappers

class FieldWrapper {
  public:
    virtual ~FieldWrapper() = default;
    virtual wxString getValue() = 0;
    virtual std::vector<wxString> getValues() = 0;
    virtual void setValues(const std::vector<wxString> &values) = 0;
};

class TextFieldWrapper : public FieldWrapper {
    wxTextCtrl *control;
  public:
    explicit TextFieldWrapper(wxTextCtrl *control) : control(control) {}
    wxString getValue() override { return control->GetValue(); }
    std::vector<wxString> getValues() override { return {getValue()}; }
    void setValues(const std::vector<wxString> &values) override {
        control->ChangeValue(values.empty() ? wxString() : values[0]);
    }
};

class ChoiceFieldWrapper : public FieldWrapper {
    wxChoice *control;
  public:
    explicit ChoiceFieldWrapper(wxChoice *control) : control(control) {}
    wxString getValue() override { return control->GetStringSelection(); }
    std::vector<wxString> getValues() override {
        return control->GetSelection() == wxNOT_FOUND ? std::vector<wxString>() : std::vector<wxString>{getValue()};
    }
    void setValues(const std::vector<wxString> &values) override {
        control->SetSelection(values.empty() ? wxNOT_FOUND : control->FindString(values[0], true));
    }
};

class ListFieldWrapper : public FieldWrapper {
    wxListBox *control;
    wxString separator;
  public:
    explicit ListFieldWrapper(wxListBox *control, wxString separator)
        : control(control), separator(separator) {}
    std::vector<wxString> getValues() override {
        wxArrayInt selections;
        control->GetSelections(selections);
        std::vector<wxString> result;
        for (auto selected : selections) result.push_back(control->GetString(selected));
        return result;
    }
    wxString getValue() override {
        wxString result;
        auto selected = getValues();
        for (size_t i = 0; i < selected.size(); i++) {
            if (i) result.Append(separator);
            result.Append(selected[i]);
        }
        return result;
    }
    void setValues(const std::vector<wxString> &values) override {
        control->DeselectAll();
        for (const auto &value : values) {
            // Preserve case and repeated equal selections; FindString defaults
            // to case-insensitive matching and would silently change the draft.
            for (unsigned int index = 0; index < control->GetCount(); index++) {
                if (control->GetString(index) == value && !control->IsSelected(index)) {
                    control->SetSelection(index);
                    break;
                }
            }
        }
    }
};

// App Code

class FormApp : public wxApp {
  public:
    virtual bool OnInit();
};
class FormFrame : public wxFrame {
  public:
    FormFrame(const wxString &title, const wxPoint &pos, const wxSize &size);

    wxPanel *panel;
    std::vector<void *> fields;
    std::unordered_map<std::string, std::unique_ptr<FieldWrapper>> idMap;
    std::unordered_map<std::string, std::vector<wxString>> defaults;
    std::unordered_map<std::string, std::vector<wxString>> saved;
    wxStaticText *draftStatus = nullptr;
    wxButton *resume = nullptr;
    wxTimer saveTimer;
    bool finishing = false;
    bool mayReplaceDraft = true;
    wxButton *submit;
    wxStaticText *helpText;
    bool hasFocusedMultilineControl;

  private:
    void AddComponent(wxPanel *parent, wxBoxSizer *sizer, FieldMetadata meta);
    void Submit(int action = 1);
    void CollectResult();
    bool SaveDraft();
    void OnChanged(wxCommandEvent &event);
    void OnSaveTimer(wxTimerEvent &event);
    void OnClose(wxCloseEvent &event);
    void OnStartFresh(wxCommandEvent &event);
    void OnResume(wxCommandEvent &event);
    void OnInsertBtn(wxCommandEvent &event);
    void OnSubmitBtn(wxCommandEvent &event);
    void OnCharHook(wxKeyEvent &event);
    void OnListBoxEvent(wxCommandEvent &event);
    void UpdateHelpText();
    void HandleNormalFocus(wxFocusEvent &event);
    void HandleMultilineFocus(wxFocusEvent &event);
};
enum { ID_Submit = 20000, ID_Insert, ID_Fresh, ID_Resume, ID_SaveTimer };

bool FormApp::OnInit() {
    const wxSize &maxFormSize =
        wxSize(formMetadata->maxWindowWidth, formMetadata->maxWindowHeight);
    FormFrame *frame =
        new FormFrame(wxString::FromUTF8(formMetadata->windowTitle),
                      wxPoint(50, 50), maxFormSize);
    frame->SetMaxSize(maxFormSize);
    setFrameIcon(wxString::FromUTF8(formMetadata->iconPath), frame);
    frame->Show(true);

    Activate(frame);

    return true;
}
FormFrame::FormFrame(const wxString &title, const wxPoint &pos,
                     const wxSize &size)
    : wxFrame(NULL, wxID_ANY, title, pos, size, DEFAULT_STYLE), saveTimer(this, ID_SaveTimer) {
    hasFocusedMultilineControl = false;

    panel = new wxPanel(this, wxID_ANY);
    wxBoxSizer *vbox = new wxBoxSizer(wxVERTICAL);
    panel->SetSizer(vbox);

    for (int field = 0; field < formMetadata->fieldSize; field++) {
        FieldMetadata meta = formMetadata->fields[field];
        AddComponent(panel, vbox, meta);
    }

    if (recoveryMetadata->enabled) {
        for (auto &field : idMap) defaults[field.first] = field.second->getValues();
        for (int i = 0; i < recoveryMetadata->savedValueSize; i++) {
            const auto &pair = recoveryMetadata->savedValues[i];
            auto &entry = saved[pair.id];
            if (pair.value) entry.push_back(wxString::FromUTF8(pair.value));
        }
        for (auto &field : idMap) {
            auto previous = saved.find(field.first);
            if (previous != saved.end()) field.second->setValues(previous->second);
        }
        draftStatus = new wxStaticText(panel, wxID_ANY,
            recoveryMetadata->hasDraft ? "Saved entries restored." : "Entries are saved locally as you type.");
        if (recoveryMetadata->draftError && strlen(recoveryMetadata->draftError)) {
            draftStatus->SetLabel(wxString::FromUTF8(recoveryMetadata->draftError) + " Use Start fresh to replace the saved draft.");
            mayReplaceDraft = false;
        }
        draftStatus->Wrap(size.GetWidth() - 2 * PADDING);
        vbox->Add(draftStatus, 0, wxEXPAND | wxALL, PADDING);
        auto recoveryButtons = new wxBoxSizer(wxHORIZONTAL);
        if (recoveryMetadata->hasDraft) {
            resume = new wxButton(panel, ID_Resume, "Resume saved");
            recoveryButtons->Add(resume, 1, wxALL, PADDING);
            Bind(wxEVT_BUTTON, &FormFrame::OnResume, this, ID_Resume);
        }
        auto fresh = new wxButton(panel, ID_Fresh, "Start fresh");
        recoveryButtons->Add(fresh, 1, wxALL, PADDING);
        vbox->Add(recoveryButtons, 0, wxEXPAND);
        Bind(wxEVT_BUTTON, &FormFrame::OnStartFresh, this, ID_Fresh);
        Bind(wxEVT_TEXT, &FormFrame::OnChanged, this);
        Bind(wxEVT_CHOICE, &FormFrame::OnChanged, this);
        Bind(wxEVT_LISTBOX, &FormFrame::OnChanged, this);
        Bind(wxEVT_TIMER, &FormFrame::OnSaveTimer, this, ID_SaveTimer);
    }

    auto actions = new wxBoxSizer(wxHORIZONTAL);
    if (recoveryMetadata->enabled) {
        auto insert = new wxButton(panel, ID_Insert, "Insert");
        actions->Add(insert, 1, wxALL, PADDING);
        Bind(wxEVT_BUTTON, &FormFrame::OnInsertBtn, this, ID_Insert);
    }
    submit = new wxButton(panel, ID_Submit, recoveryMetadata->enabled ? "Copy result" : "Submit");
    submit->SetDefault();
    actions->Add(submit, 1, wxALL, PADDING);
    vbox->Add(actions, 0, wxEXPAND);

    helpText =
        new wxStaticText(panel, wxID_ANY, "", wxDefaultPosition, wxDefaultSize);
    wxFont helpFont = helpText->GetFont();
    helpFont.SetPointSize(8);
    helpText->SetFont(helpFont);
    vbox->Add(helpText, 0, wxLEFT | wxRIGHT | wxBOTTOM, PADDING);
    UpdateHelpText();

    Bind(wxEVT_BUTTON, &FormFrame::OnSubmitBtn, this, ID_Submit);
    Bind(wxEVT_CHAR_HOOK, &FormFrame::OnCharHook, this, wxID_ANY);
    Bind(wxEVT_CLOSE_WINDOW, &FormFrame::OnClose, this);

    this->SetClientSize(panel->GetBestSize());
    this->CentreOnScreen();
}

void FormFrame::AddComponent(wxPanel *parent, wxBoxSizer *sizer,
                             FieldMetadata meta) {
    void *control = nullptr;

    switch (meta.fieldType) {
    case FieldType::LABEL: {
        const LabelMetadata *labelMeta =
            static_cast<const LabelMetadata *>(meta.specific);
        const long style = wxST_ELLIPSIZE_END;
        auto label = new wxStaticText(parent, wxID_ANY,
                                      wxString::FromUTF8(labelMeta->text),
                                      wxDefaultPosition, wxDefaultSize, style);

        label->Wrap(this->GetClientSize().GetWidth());
        control = label;
        fields.push_back(label);
        break;
    }
    case FieldType::TEXT: {
        const TextMetadata *textMeta =
            static_cast<const TextMetadata *>(meta.specific);
        long style = 0;
        if (textMeta->multiline) {
            style |= wxTE_MULTILINE;
        }

        auto textControl = new wxTextCtrl(
            parent, NewControlId(), wxString::FromUTF8(textMeta->defaultText),
            wxDefaultPosition, wxDefaultSize, style);

        if (textMeta->multiline) {
            textControl->SetMinSize(
                wxSize(MULTILINE_MIN_WIDTH, MULTILINE_MIN_HEIGHT));
            textControl->Bind(wxEVT_SET_FOCUS, &FormFrame::HandleMultilineFocus,
                              this, wxID_ANY);
        } else {
            textControl->Bind(wxEVT_SET_FOCUS, &FormFrame::HandleNormalFocus,
                              this, wxID_ANY);
        }

        // Create the field wrapper
        std::unique_ptr<FieldWrapper> field(
            new TextFieldWrapper(textControl));
        idMap[meta.id] = std::move(field);
        control = textControl;
        fields.push_back(textControl);
        break;
    }
    case FieldType::CHOICE: {
        const ChoiceMetadata *choiceMeta =
            static_cast<const ChoiceMetadata *>(meta.specific);

        int selectedItem = -1;
        wxArrayString choices;
        for (int i = 0; i < choiceMeta->valueSize; i++) {
            choices.Add(wxString::FromUTF8(choiceMeta->values[i]));

            if (strcmp(choiceMeta->values[i], choiceMeta->defaultValue) == 0) {
                selectedItem = i;
            }
        }

        void *choice = nullptr;
        wxString separator = wxString::FromUTF8(choiceMeta->separator);
        if (choiceMeta->choiceType == ChoiceType::DROPDOWN) {
            choice = (void *)new wxChoice(parent, wxID_ANY, wxDefaultPosition,
                                          wxDefaultSize, choices);

            if (selectedItem >= 0) {
                ((wxChoice *)choice)->SetSelection(selectedItem);
            }

            ((wxChoice *)choice)
                ->Bind(wxEVT_SET_FOCUS, &FormFrame::HandleNormalFocus, this,
                       wxID_ANY);

            // Create the field wrapper
            std::unique_ptr<FieldWrapper> field(
                new ChoiceFieldWrapper((wxChoice *)choice));
            idMap[meta.id] = std::move(field);
        } else {
            choice = (void *)new wxListBox(parent, wxID_ANY, wxDefaultPosition,
                                           wxDefaultSize, choices,
                                           wxLB_EXTENDED);

            if (selectedItem >= 0) {
                ((wxListBox *)choice)->SetSelection(selectedItem);
            }

            ((wxListBox *)choice)
                ->Bind(wxEVT_SET_FOCUS, &FormFrame::HandleNormalFocus, this,
                       wxID_ANY);
            // ListBoxes prevent the global CHAR_HOOK handler from handling the
            // Return key correctly, so we need to handle the double click event
            // too (which is triggered when the enter key is pressed). See:
            // https://github.com/espanso/espanso/issues/857
            ((wxListBox *)choice)
                ->Bind(wxEVT_LISTBOX_DCLICK, &FormFrame::OnListBoxEvent, this,
                       wxID_ANY);

            // Create the field wrapper
            std::unique_ptr<FieldWrapper> field(
                new ListFieldWrapper((wxListBox *)choice,
                                                     separator));
            idMap[meta.id] = std::move(field);
        }

        control = choice;
        fields.push_back(choice);
        break;
    }
    case FieldType::ROW: {
        const RowMetadata *rowMeta =
            static_cast<const RowMetadata *>(meta.specific);

        auto innerPanel = new wxPanel(panel, wxID_ANY);
        wxBoxSizer *hbox = new wxBoxSizer(wxHORIZONTAL);
        innerPanel->SetSizer(hbox);
        sizer->Add(innerPanel, 0, wxEXPAND | wxALL, 0);
        fields.push_back(innerPanel);

        for (int field = 0; field < rowMeta->fieldSize; field++) {
            FieldMetadata innerMeta = rowMeta->fields[field];
            AddComponent(innerPanel, hbox, innerMeta);
        }

        break;
    }
    default:
        // TODO: handle unknown field type
        break;
    }

    if (control) {
        sizer->Add((wxWindow *)control, 0, wxEXPAND | wxALL, PADDING);
    }
}

void FormFrame::CollectResult() {
    for (auto &field : idMap) {
        wxCharBuffer buffer = field.second->getValue().ToUTF8();
        values.push_back({strdup(field.first.c_str()), strdup(buffer.data())});
    }
}

bool FormFrame::SaveDraft() {
    if (!recoveryMetadata->enabled) return true;
    saveTimer.Stop();
    if (!mayReplaceDraft) return false;
    std::vector<ValuePair> snapshot;
    for (auto &field : idMap) {
        auto fieldValues = field.second->getValues();
        if (fieldValues.empty()) snapshot.push_back({strdup(field.first.c_str()), nullptr});
        for (const auto &value : fieldValues) {
            snapshot.push_back({strdup(field.first.c_str()), strdup(value.ToUTF8().data())});
        }
    }
    bool success = saveDraftCallback(snapshot.data(), snapshot.size(), formCallbackData) != 0;
    for (auto &pair : snapshot) { free((void *)pair.id); free((void *)pair.value); }
    if (draftStatus) {
        draftStatus->SetLabel(success ? "Entries saved locally." : "Could not save entries. Keep this form open to avoid losing them.");
        draftStatus->Wrap(formMetadata->maxWindowWidth - 2 * PADDING);
        panel->Layout();
    }
    return success;
}

void FormFrame::Submit(int action) {
    if (finishing) return;
    if (!SaveDraft() && wxMessageBox("Your entries could not be saved for recovery. Continue anyway?",
            "Form entries not saved", wxYES_NO | wxNO_DEFAULT | wxICON_WARNING, this) != wxYES) return;
    formAction = recoveryMetadata->enabled ? action : 2;
    CollectResult();
    finishing = true;
    Close(true);
}

void FormFrame::OnChanged(wxCommandEvent &event) {
    if (resume) resume->Disable();
    saveTimer.StartOnce(250);
    event.Skip();
}

void FormFrame::OnSaveTimer(wxTimerEvent &event) { SaveDraft(); }

void FormFrame::OnClose(wxCloseEvent &event) {
    if (!finishing && recoveryMetadata->enabled) {
        if (!SaveDraft() && event.CanVeto() && wxMessageBox(
                "Your entries could not be saved. Close and lose these changes?",
                "Form entries not saved", wxYES_NO | wxNO_DEFAULT | wxICON_WARNING, this) != wxYES) {
            event.Veto();
            return;
        }
        CollectResult();
    }
    saveTimer.Stop();
    event.Skip();
}

void FormFrame::OnStartFresh(wxCommandEvent &event) {
    if (wxMessageBox("Replace these entries with the form defaults?", "Start fresh",
                    wxYES_NO | wxNO_DEFAULT | wxICON_QUESTION, this) != wxYES) return;
    mayReplaceDraft = true;
    if (resume) resume->Disable();
    for (auto &field : idMap) field.second->setValues(defaults[field.first]);
    SaveDraft();
}

void FormFrame::OnResume(wxCommandEvent &event) {
    if (resume) resume->Disable();
    for (auto &field : idMap) {
        auto previous = saved.find(field.first);
        field.second->setValues(previous == saved.end() ? defaults[field.first] : previous->second);
    }
    SaveDraft();
}

void FormFrame::OnInsertBtn(wxCommandEvent &event) { Submit(2); }

void FormFrame::HandleNormalFocus(wxFocusEvent &event) {
    hasFocusedMultilineControl = false;
    UpdateHelpText();
    event.Skip();
}

void FormFrame::HandleMultilineFocus(wxFocusEvent &event) {
    hasFocusedMultilineControl = true;
    UpdateHelpText();
    event.Skip();
}

void FormFrame::UpdateHelpText() {
    if (hasFocusedMultilineControl) {
        helpText->SetLabel(recoveryMetadata->enabled ? "CTRL+Enter copies the result. ESC keeps your draft." : "(or press CTRL+Enter to submit, ESC to cancel)");
    } else {
        helpText->SetLabel(recoveryMetadata->enabled ? "Enter copies the result. ESC keeps your draft." : "(or press Enter to submit, ESC to cancel)");
    }
    this->SetClientSize(panel->GetBestSize());
}

void FormFrame::OnSubmitBtn(wxCommandEvent &event) { Submit(); }

void FormFrame::OnCharHook(wxKeyEvent &event) {
    if (event.GetKeyCode() == WXK_ESCAPE) {
        Close();
    } else if (event.GetKeyCode() == WXK_RETURN) {
        if (!hasFocusedMultilineControl || wxGetKeyState(WXK_RAW_CONTROL)) {
            Submit();
        } else {
            event.Skip();
        }
    } else {
        event.Skip();
    }
}

void FormFrame::OnListBoxEvent(wxCommandEvent &event) { Submit(); }

extern "C" void interop_show_form(FormMetadata *_metadata,
                                  const FormRecoveryMetadata *_recovery,
                                  void (*callback)(ValuePair *values, int size, int action, void *data),
                                  int (*save_draft)(ValuePair *values, int size, void *data),
                                  void *data) {
// Setup high DPI support on Windows
#ifdef __WXMSW__
    SetProcessDPIAware();
#endif

    formMetadata = _metadata;
    recoveryMetadata = _recovery;
    saveDraftCallback = save_draft;
    formCallbackData = data;
    formAction = 0;
    values.clear();

    wxApp::SetInstance(new FormApp());
    int argc = 0;
    wxEntry(argc, (char **)nullptr);

    callback(values.data(), values.size(), formAction, data);

    // Free up values
    for (auto pair : values) {
        free((void *)pair.id);
        free((void *)pair.value);
    }
}
