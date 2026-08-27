use egui::{FontId, TextFormat, text::LayoutJob};

use crate::{models::Message, ui::theme};

pub const COMPOSER_MIN_HEIGHT: f32 = 72.0;
pub const COMPOSER_MAX_HEIGHT: f32 = 180.0;
const COMPOSER_LINE_HEIGHT: f32 = 19.0;
const COMPOSER_BASE_LINES: usize = 3;
const COMPOSER_FIXED_HEIGHT: f32 = 28.0;
const MIN_HISTORY_HEIGHT: f32 = 96.0;
pub const COMPOSER_ACTION_WIDTH: f32 = 92.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ComposerLayout {
    pub editor_height: f32,
    pub footer_height: f32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComposerAction {
    pub send: bool,
    pub stop: bool,
}

pub fn composer_layout(text: &str, available_height: f32) -> ComposerLayout {
    let line_count = text.split('\n').count().max(1);
    let extra_lines = line_count.saturating_sub(COMPOSER_BASE_LINES) as f32;
    let desired_height =
        (COMPOSER_MIN_HEIGHT + extra_lines * COMPOSER_LINE_HEIGHT).min(COMPOSER_MAX_HEIGHT);
    let available_editor_height = (available_height - MIN_HISTORY_HEIGHT - COMPOSER_FIXED_HEIGHT)
        .clamp(COMPOSER_MIN_HEIGHT, COMPOSER_MAX_HEIGHT);
    let editor_height = desired_height.min(available_editor_height);
    ComposerLayout {
        editor_height,
        footer_height: editor_height + COMPOSER_FIXED_HEIGHT,
    }
}

pub fn composer(
    ui: &mut egui::Ui,
    text: &mut String,
    id: egui::Id,
    layout: ComposerLayout,
    generating: bool,
    hint: &str,
) -> ComposerAction {
    let mut action = ComposerAction::default();
    ui.set_height(layout.footer_height);
    ui.horizontal(|ui| {
        let visual_gutter = 2.0;
        let input_width = (ui.available_width()
            - COMPOSER_ACTION_WIDTH
            - ui.spacing().item_spacing.x
            - visual_gutter * 2.0)
            .max(120.0);
        ui.add_space(visual_gutter);
        let response = ui
            .allocate_ui_with_layout(
                egui::vec2(input_width, layout.editor_height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_height(layout.editor_height);
                    egui::ScrollArea::vertical()
                        .id_salt(id.with("scroll"))
                        .auto_shrink([false, false])
                        .max_height(layout.editor_height)
                        .min_scrolled_height(layout.editor_height)
                        .show(ui, |ui| {
                            let output = egui::TextEdit::multiline(text)
                                .id(id.with("text"))
                                .hint_text(hint)
                                .desired_rows(COMPOSER_BASE_LINES)
                                .desired_width(f32::INFINITY)
                                .min_size(egui::vec2(
                                    input_width - ui.spacing().scroll.bar_width - 4.0,
                                    layout.editor_height - 4.0,
                                ))
                                .return_key(egui::KeyboardShortcut::new(
                                    egui::Modifiers::SHIFT,
                                    egui::Key::Enter,
                                ))
                                .show(ui);
                            let keyboard_navigation = ui.input(|input| {
                                [
                                    egui::Key::ArrowDown,
                                    egui::Key::ArrowUp,
                                    egui::Key::PageDown,
                                    egui::Key::PageUp,
                                    egui::Key::Home,
                                    egui::Key::End,
                                ]
                                .into_iter()
                                .any(|key| input.key_pressed(key))
                            });
                            if output.response.has_focus() && keyboard_navigation {
                                if let Some(cursor_range) = output.cursor_range {
                                    let caret =
                                        egui::text_selection::text_cursor_state::cursor_rect(
                                            &output.galley,
                                            &cursor_range.primary,
                                            ui.text_style_height(&egui::TextStyle::Body),
                                        )
                                        .translate(output.galley_pos.to_vec2())
                                        .expand(4.0);
                                    ui.scroll_to_rect(caret, None);
                                }
                            }
                            output.response
                        })
                        .inner
                },
            )
            .inner;
        let enter_send = !generating
            && response.has_focus()
            && ui.input(|input| input.key_pressed(egui::Key::Enter) && !input.modifiers.shift);
        let clicked = ui
            .add_sized(
                [COMPOSER_ACTION_WIDTH, layout.editor_height],
                if generating {
                    egui::Button::new("Detener")
                } else {
                    theme::primary_button("Enviar")
                },
            )
            .clicked();
        action.send = enter_send || (!generating && clicked);
        action.stop = generating && clicked;
        ui.add_space(visual_gutter);
    });
    ui.add_space(4.0);
    ui.colored_label(
        theme::TEXT_MUTED,
        egui::RichText::new("La calidad de las respuestas depende del modelo local seleccionado.")
            .small(),
    );
    action
}

pub fn messages(
    ui: &mut egui::Ui,
    messages: &[Message],
    pending: bool,
    streaming_text: &str,
    pending_label: Option<&str>,
    diagnostic_mode: bool,
    source_library_scope: Option<i64>,
) {
    let available_height = ui.available_height().max(1.0);
    egui::ScrollArea::vertical()
        .id_salt("chat_history_scroll")
        .auto_shrink([false, false])
        .max_height(available_height)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            if messages.is_empty() && streaming_text.is_empty() {
                ui.add_space(80.0);
                ui.vertical_centered(|ui| {
                    ui.heading("Kuznor");
                    ui.weak("Tu inteligencia local, privada y lista para trabajar.");
                });
                ui.add_space(80.0);
            }
            for message in messages {
                message_card(ui, message, diagnostic_mode, false, source_library_scope);
            }
            if !streaming_text.is_empty() {
                message_card(
                    ui,
                    &Message {
                        id: 0,
                        chat_id: 0,
                        role: "assistant".into(),
                        content: streaming_text.into(),
                        sources: Vec::new(),
                        created_at: String::new(),
                    },
                    diagnostic_mode,
                    true,
                    source_library_scope,
                );
            }
            if pending {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak(pending_label.unwrap_or(if streaming_text.is_empty() {
                        "Pensando..."
                    } else {
                        "Generando..."
                    }));
                });
            }
        });
}

fn message_card(
    ui: &mut egui::Ui,
    message: &Message,
    diagnostic_mode: bool,
    streaming: bool,
    source_library_scope: Option<i64>,
) {
    let assistant = message.role == "assistant";
    let available_width = ui.available_width();
    let card_width = if assistant {
        available_width
    } else {
        (available_width * 0.78).max(280.0)
    };
    let fill = if assistant {
        theme::SURFACE
    } else {
        theme::ACCENT_SOFT
    };

    ui.with_layout(
        if assistant {
            egui::Layout::left_to_right(egui::Align::Min)
        } else {
            egui::Layout::right_to_left(egui::Align::Min)
        },
        |ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(card_width, 1.0),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    egui::Frame::new()
                        .fill(fill)
                        .stroke(egui::Stroke::new(
                            1.0,
                            if assistant {
                                theme::BORDER
                            } else {
                                theme::ACCENT
                            },
                        ))
                        .corner_radius(theme::CORNER_RADIUS)
                        .inner_margin(egui::Margin::symmetric(14, 12))
                        .outer_margin(egui::Margin::symmetric(0, 5))
                        .show(ui, |ui| {
                            let visible_sources = message
                                .sources
                                .iter()
                                .filter(|source| {
                                    source_matches_library(source.library_id, source_library_scope)
                                })
                                .collect::<Vec<_>>();
                            ui.horizontal(|ui| {
                                ui.strong(if assistant { "ASISTENTE" } else { "TU" });
                                if !visible_sources.is_empty() {
                                    let based_on_code = visible_sources
                                        .iter()
                                        .any(|source| !source.relative_path.is_empty());
                                    ui.colored_label(
                                        theme::ACCENT_HOVER,
                                        if based_on_code {
                                            "Basada en codigo"
                                        } else {
                                            "Basada en documentos"
                                        },
                                    );
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.small_button("Copiar").clicked() {
                                            ui.ctx().copy_text(message.content.clone());
                                        }
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            if streaming {
                                ui.add(
                                    egui::Label::new(message.content.clone())
                                        .wrap_mode(egui::TextWrapMode::Wrap),
                                );
                            } else {
                                render_markdown(ui, &message.content, message.id);
                            }
                            if !visible_sources.is_empty() {
                                ui.add_space(10.0);
                                ui.separator();
                                ui.strong("Fuentes");
                                for (source_index, source) in
                                    visible_sources.into_iter().enumerate()
                                {
                                    let location = match (source.line_start, source.line_end) {
                                        (Some(start), Some(end)) => {
                                            format!("lineas {start}-{end}")
                                        }
                                        _ => source
                                            .page_number
                                            .map(|page| format!("pagina {page}"))
                                            .unwrap_or_else(|| {
                                                format!("fragmento {}", source.chunk_index + 1)
                                            }),
                                    };
                                    theme::surface_frame().show(ui, |ui| {
                                        ui.colored_label(
                                            theme::ACCENT_HOVER,
                                            if diagnostic_mode {
                                                format!(
                                                    "{} - {location} (score {:.3})",
                                                    source.document_name, source.score
                                                )
                                            } else {
                                                format!("{} - {location}", source.document_name)
                                            },
                                        );
                                        if !source.preview.is_empty() {
                                            let source_id = egui::Id::new((
                                                "source_preview",
                                                message.id,
                                                source.document_id,
                                                source.chunk_id,
                                                source_index,
                                            ));
                                            egui::CollapsingHeader::new("Ver fragmento")
                                                .id_salt(source_id)
                                                .show(ui, |ui| {
                                                    ui.weak(&source.preview);
                                                });
                                        }
                                    });
                                }
                            }
                        });
                },
            );
        },
    );
}

fn source_matches_library(source_library_id: i64, scope: Option<i64>) -> bool {
    scope.is_none_or(|library_id| source_library_id == library_id)
}

fn render_markdown(ui: &mut egui::Ui, text: &str, message_id: i64) {
    let lines = text.lines().collect::<Vec<_>>();
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        if let Some(language) = line.trim().strip_prefix("```") {
            let mut code = String::new();
            index += 1;
            while index < lines.len() && !lines[index].trim().starts_with("```") {
                code.push_str(lines[index]);
                code.push('\n');
                index += 1;
            }
            code_block(ui, index, language.trim(), &code);
        } else if index + 1 < lines.len() && is_table_separator(lines[index + 1]) {
            let headers = table_cells(line);
            let mut rows = Vec::new();
            index += 2;
            while index < lines.len()
                && lines[index].contains('|')
                && !lines[index].trim().is_empty()
            {
                rows.push(table_cells(lines[index]));
                index += 1;
            }
            table(ui, message_id, index, &headers, &rows);
            continue;
        } else if let Some(title) = line.strip_prefix("### ") {
            ui.add(egui::Label::new(inline_job(title, 16.0, true)).wrap());
        } else if let Some(title) = line.strip_prefix("## ") {
            ui.add(egui::Label::new(inline_job(title, 18.0, true)).wrap());
        } else if let Some(title) = line.strip_prefix("# ") {
            ui.add(egui::Label::new(inline_job(title, 21.0, true)).wrap());
        } else if let Some(quote) = line.strip_prefix("> ") {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::ACCENT))
                .inner_margin(8)
                .show(ui, |ui| {
                    ui.add(egui::Label::new(inline_job(quote, 14.0, false)).wrap());
                });
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            ui.horizontal(|ui| {
                ui.label("-");
                ui.add(egui::Label::new(inline_job(item, 14.0, false)).wrap());
            });
        } else if let Some((number, item)) = numbered_item(line) {
            ui.horizontal(|ui| {
                ui.label(format!("{number}."));
                ui.add(egui::Label::new(inline_job(item, 14.0, false)).wrap());
            });
        } else if line.trim().is_empty() {
            ui.add_space(5.0);
        } else {
            linked_line(ui, line);
        }
        index += 1;
    }
}

fn linked_line(ui: &mut egui::Ui, line: &str) {
    let Some(open) = line.find('[') else {
        ui.add(egui::Label::new(inline_job(line, 14.0, false)).wrap());
        return;
    };
    let Some(close_relative) = line[open + 1..].find("](") else {
        ui.add(egui::Label::new(inline_job(line, 14.0, false)).wrap());
        return;
    };
    let close = open + 1 + close_relative;
    let url_start = close + 2;
    let Some(url_end_relative) = line[url_start..].find(')') else {
        ui.add(egui::Label::new(inline_job(line, 14.0, false)).wrap());
        return;
    };
    let url_end = url_start + url_end_relative;
    ui.horizontal_wrapped(|ui| {
        if open > 0 {
            ui.add(egui::Label::new(inline_job(&line[..open], 14.0, false)));
        }
        ui.colored_label(theme::ACCENT_HOVER, &line[open + 1..close])
            .on_hover_text(format!(
                "Enlace desactivado en el modo privado local: {}",
                &line[url_start..url_end]
            ));
        if url_end + 1 < line.len() {
            ui.add(egui::Label::new(inline_job(
                &line[url_end + 1..],
                14.0,
                false,
            )));
        }
    });
}

fn inline_job(text: &str, size: f32, heading: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    let normal = TextFormat {
        font_id: FontId::proportional(size),
        color: theme::TEXT,
        ..Default::default()
    };
    let strong = TextFormat {
        font_id: FontId::proportional(size),
        color: theme::TEXT,
        ..Default::default()
    };
    let code = TextFormat {
        font_id: FontId::monospace(size - 1.0),
        color: theme::ACCENT_HOVER,
        background: theme::CODE_BACKGROUND,
        ..Default::default()
    };
    if heading {
        job.append(text, 0.0, strong);
        return job;
    }

    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            if let Some(end) = after.find("**") {
                job.append(&after[..end], 0.0, strong.clone());
                rest = &after[end + 2..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('`') {
            if let Some(end) = after.find('`') {
                job.append(&after[..end], 0.0, code.clone());
                rest = &after[end + 1..];
                continue;
            }
        }
        if let Some(after) = rest.strip_prefix('*') {
            if let Some(end) = after.find('*') {
                job.append(
                    &after[..end],
                    0.0,
                    TextFormat {
                        italics: true,
                        ..normal.clone()
                    },
                );
                rest = &after[end + 1..];
                continue;
            }
        }
        let next = rest
            .char_indices()
            .skip(1)
            .find_map(|(position, character)| matches!(character, '*' | '`').then_some(position))
            .unwrap_or(rest.len());
        job.append(&rest[..next], 0.0, normal.clone());
        rest = &rest[next..];
    }
    job
}

fn markdown_table_id(message_id: i64, table_index: usize) -> egui::Id {
    egui::Id::new(("markdown_table", message_id, table_index))
}

fn table(
    ui: &mut egui::Ui,
    message_id: i64,
    table_index: usize,
    headers: &[String],
    rows: &[Vec<String>],
) {
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(theme::CORNER_RADIUS)
        .inner_margin(8)
        .show(ui, |ui| {
            egui::Grid::new(markdown_table_id(message_id, table_index))
                .striped(true)
                .min_col_width(90.0)
                .show(ui, |ui| {
                    for header in headers {
                        ui.strong(header);
                    }
                    ui.end_row();
                    for row in rows {
                        for cell in row {
                            ui.add(egui::Label::new(inline_job(cell, 13.0, false)).wrap());
                        }
                        ui.end_row();
                    }
                });
        });
}

fn table_cells(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_owned())
        .collect()
}

fn is_table_separator(line: &str) -> bool {
    let compact = line
        .trim()
        .trim_matches('|')
        .replace(':', "")
        .replace(' ', "");
    !compact.is_empty()
        && compact
            .split('|')
            .all(|cell| cell.chars().all(|character| character == '-'))
}

fn numbered_item(line: &str) -> Option<(&str, &str)> {
    let (number, item) = line.split_once(". ")?;
    number
        .chars()
        .all(|character| character.is_ascii_digit())
        .then_some((number, item))
}

fn code_block(ui: &mut egui::Ui, block_index: usize, language: &str, code: &str) {
    ui.push_id(("markdown_code", block_index), |ui| {
        egui::Frame::new()
            .fill(theme::CODE_BACKGROUND)
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .inner_margin(12)
            .corner_radius(theme::CORNER_RADIUS)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.colored_label(
                        theme::ACCENT_HOVER,
                        if language.is_empty() {
                            "CODIGO".to_owned()
                        } else {
                            language.to_ascii_uppercase()
                        },
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Copiar codigo").clicked() {
                            ui.ctx().copy_text(code.to_owned());
                        }
                    });
                });
                ui.add_space(6.0);
                egui::ScrollArea::horizontal()
                    .id_salt(ui.id().with("code_scroll"))
                    .show(ui, |ui| {
                        for line in code.lines() {
                            ui.add(
                                egui::Label::new(code_job(line, language))
                                    .wrap_mode(egui::TextWrapMode::Extend),
                            );
                        }
                    });
            });
    });
}

fn code_job(line: &str, language: &str) -> LayoutJob {
    let mut job = LayoutJob::default();
    let normal = TextFormat {
        font_id: FontId::monospace(13.0),
        color: theme::TEXT,
        ..Default::default()
    };
    let keyword = TextFormat {
        font_id: FontId::monospace(13.0),
        color: theme::ACCENT_HOVER,
        ..Default::default()
    };
    let string = TextFormat {
        font_id: FontId::monospace(13.0),
        color: theme::WARNING,
        ..Default::default()
    };
    let comment = TextFormat {
        font_id: FontId::monospace(13.0),
        color: theme::SUCCESS,
        italics: true,
        ..Default::default()
    };
    if line.trim_start().starts_with("//")
        || line.trim_start().starts_with('#')
        || line.trim_start().starts_with("--")
    {
        job.append(line, 0.0, comment);
        return job;
    }
    let keywords: &[&str] = if language.eq_ignore_ascii_case("sql") {
        &[
            "SELECT", "FROM", "WHERE", "JOIN", "LEFT", "RIGHT", "ON", "GROUP", "BY", "ORDER", "AS",
            "WITH", "INSERT", "UPDATE", "DELETE",
        ]
    } else if language.eq_ignore_ascii_case("python") {
        &[
            "def", "class", "return", "import", "from", "if", "else", "for", "while", "in", "True",
            "False", "None",
        ]
    } else {
        &[
            "fn", "let", "mut", "pub", "struct", "impl", "use", "return", "if", "else", "match",
            "for", "while", "async", "await",
        ]
    };
    for word in
        line.split_inclusive(|character: char| !character.is_alphanumeric() && character != '_')
    {
        let trimmed =
            word.trim_matches(|character: char| !character.is_alphanumeric() && character != '_');
        let format = if keywords
            .iter()
            .any(|keyword| keyword.eq_ignore_ascii_case(trimmed))
        {
            keyword.clone()
        } else if word.contains('"') || word.contains('\'') {
            string.clone()
        } else {
            normal.clone()
        };
        job.append(word, 0.0, format);
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composer_height_tracks_current_content_with_a_hard_cap() {
        let one_line = composer_layout("hola", 600.0);
        let five_lines = composer_layout("1\n2\n3\n4\n5", 600.0);
        let fifty_lines = composer_layout(&vec!["linea"; 50].join("\n"), 600.0);
        let cleared = composer_layout("", 600.0);

        assert_eq!(one_line.editor_height, COMPOSER_MIN_HEIGHT);
        assert!(five_lines.editor_height > COMPOSER_MIN_HEIGHT);
        assert!(five_lines.editor_height < COMPOSER_MAX_HEIGHT);
        assert_eq!(fifty_lines.editor_height, COMPOSER_MAX_HEIGHT);
        assert_eq!(cleared.editor_height, COMPOSER_MIN_HEIGHT);
    }

    #[test]
    fn composer_reserves_history_actions_and_quality_notice() {
        let available = 420.0;
        let layout = composer_layout(&vec!["linea"; 50].join("\n"), available);
        assert!(layout.editor_height <= COMPOSER_MAX_HEIGHT);
        assert!(layout.footer_height <= available - MIN_HISTORY_HEIGHT);
        assert_eq!(COMPOSER_ACTION_WIDTH, 92.0);
        assert_eq!(
            layout.footer_height - layout.editor_height,
            COMPOSER_FIXED_HEIGHT
        );
    }

    #[test]
    fn arithmetic_asterisk_is_preserved_as_literal_text() {
        let job = inline_job("$100 * 0.25", 14.0, false);
        assert_eq!(job.text, "$100 * 0.25");
    }

    #[test]
    fn markdown_tables_have_contextual_unique_ids() {
        assert_ne!(markdown_table_id(10, 1), markdown_table_id(10, 2));
        assert_ne!(markdown_table_id(10, 1), markdown_table_id(11, 1));
        assert_eq!(markdown_table_id(10, 1), markdown_table_id(10, 1));
    }

    #[test]
    fn historical_sources_are_not_presented_as_current_in_another_library() {
        assert!(source_matches_library(10, Some(10)));
        assert!(!source_matches_library(10, Some(20)));
        assert!(source_matches_library(10, None));
    }
}
