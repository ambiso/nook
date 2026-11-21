use crate::{
    Choice, Profile, Result, choices, delete_instance, new_ephemeral_id, parse_name,
    prepare_deletion, request_stop, storage::SavedInstance,
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    io::{self, IsTerminal},
    time::{Duration, Instant},
};

struct NamePrompt {
    profile: usize,
    name: String,
    error: Option<String>,
}

struct DeletePrompt {
    choice: Choice,
    saved: SavedInstance,
    error: Option<String>,
}

pub fn running_indicator(running: bool) -> Span<'static> {
    if running {
        Span::styled("● ", Style::new().fg(Color::Green))
    } else {
        Span::raw("  ")
    }
}

fn matching_indices(entries: &[Choice], profiles: &[Profile], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..entries.len()).collect();
    }
    let query = query.to_lowercase();
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, choice)| {
            let name = profiles[choice.profile].values["name"].to_lowercase();
            let label = format!("{} {}", name, choice.instance.to_lowercase());
            label.contains(&query).then_some(index)
        })
        .collect()
}

impl NamePrompt {
    fn submit(&self, entries: &[Choice]) -> std::result::Result<Choice, String> {
        let name = parse_name(&self.name)?;
        let instance = format!("n-{name}");
        if entries
            .iter()
            .any(|entry| entry.profile == self.profile && entry.instance == instance)
        {
            return Err(
                "That name already exists for this template; select it from the list.".into(),
            );
        }
        Ok(Choice {
            profile: self.profile,
            instance,
            running: false,
        })
    }
}

pub fn select(profiles: &[Profile]) -> Result<Option<Choice>> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("use nk list, or nk launch PROFILE [--ephemeral] [-- COMMAND ...]".into());
    }
    // Ratatui restores raw mode, cursor visibility and the alternate screen
    // after normal errors and panics, before a selected shell takes over.
    ratatui::run(|terminal| {
        let mut entries = choices(profiles)?;
        let mut selection = ListState::default().with_selected(Some(0));
        let mut refreshed = Instant::now();
        let mut prompt: Option<NamePrompt> = None;
        let mut deletion: Option<DeletePrompt> = None;
        let mut status: Option<String> = None;
        let mut query = String::new();
        let mut search_draft: Option<String> = None;
        loop {
            if entries.is_empty() {
                return Err("no sandbox profiles are configured".into());
            }
            let active_query = search_draft.as_deref().unwrap_or(&query);
            let visible = matching_indices(&entries, profiles, active_query);
            let selected = (!visible.is_empty())
                .then(|| selection.selected().unwrap_or(0).min(visible.len() - 1));
            selection.select(selected);
            terminal.draw(|frame| {
                let [list_area, help_area] =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(3)])
                        .areas(frame.area());
                let rows = visible.iter().map(|&index| {
                    let choice = &entries[index];
                    let text = format!(
                        "{:<24} {:<22} {}",
                        profiles[choice.profile].values["name"],
                        choice.instance,
                        if choice.running { "running" } else { "stopped" },
                    );
                    ListItem::new(Line::from(vec![
                        running_indicator(choice.running),
                        Span::raw(text),
                    ]))
                });
                let list = List::new(rows)
                    .block(Block::bordered().title(format!(
                        " Nook ({}/{}) ",
                        visible.len(),
                        entries.len()
                    )))
                    .highlight_symbol("› ")
                    .highlight_style(Style::new().fg(Color::Cyan));
                frame.render_stateful_widget(list, list_area, &mut selection);
                let search_line = if search_draft.is_some() {
                    format!("/{active_query}_   Enter: apply   Esc: cancel   Ctrl+U: clear")
                } else if query.is_empty() {
                    "/: search by profile or instance".into()
                } else {
                    format!("/{query}   /: edit   Esc: clear")
                };
                let help = status.as_deref().unwrap_or(if visible.is_empty() {
                    "No matching instances"
                } else {
                    "↑/↓ j/k: select   Enter: launch/enter   n: named   e: ephemeral   s: stop   d: delete   q: quit"
                });
                frame.render_widget(
                    Paragraph::new(format!("{search_line}\n{help}")).wrap(Wrap { trim: false }),
                    help_area,
                );
                if let Some(prompt) = &prompt {
                    let area = frame.area();
                    let width = area.width.saturating_sub(4).min(76);
                    let height = area.height.min(9);
                    let popup = Rect::new(
                        area.x + (area.width - width) / 2,
                        area.y + (area.height - height) / 2,
                        width,
                        height,
                    );
                    frame.render_widget(Clear, popup);
                    let template = &profiles[prompt.profile].values["name"];
                    let help = prompt.error.as_deref().unwrap_or(
                        "1–24 lowercase letters, digits or hyphens. Enter: create; Esc: cancel.",
                    );
                    frame.render_widget(
                        Paragraph::new(format!(
                            "Name: {}_\n\n{help}\n\nClean persistent home; explicit template mounts stay shared.",
                            prompt.name,
                        ))
                        .block(Block::bordered().title(format!(
                            " New persistent instance · template: {template} "
                        )))
                        .wrap(Wrap { trim: false }),
                        popup,
                    );
                }
                if let Some(deletion) = &deletion {
                    let area = frame.area();
                    let width = area.width.saturating_sub(4).min(76);
                    let height = area.height.min(9);
                    let popup = Rect::new(
                        area.x + (area.width - width) / 2,
                        area.y + (area.height - height) / 2,
                        width,
                        height,
                    );
                    let choice = &deletion.choice;
                    let help = deletion.error.as_deref().unwrap_or(
                        "All saved data will be permanently removed. y: delete; n/Esc: cancel.",
                    );
                    frame.render_widget(Clear, popup);
                    frame.render_widget(
                        Paragraph::new(format!(
                            "{}/{}\n{:?}\n\n{help}",
                            profiles[choice.profile].get("name").unwrap_or("?"),
                            choice.instance,
                            deletion.saved.path,
                        ))
                        .block(Block::bordered().title(" Delete saved instance? "))
                        .wrap(Wrap { trim: false }),
                        popup,
                    );
                }
            })?;
            if event::poll(Duration::from_millis(250))?
                && let Event::Key(key) = event::read()?
            {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return Ok(None);
                }
                if let Some(input) = &mut deletion {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('n') => deletion = None,
                        KeyCode::Char('y')
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            let choice = &input.choice;
                            match delete_instance(&profiles[choice.profile], &choice.instance) {
                                Ok(()) => {
                                    deletion = None;
                                    entries = choices(profiles)?;
                                }
                                Err(error) => input.error = Some(error.to_string()),
                            }
                        }
                        _ => (),
                    }
                    continue;
                }
                if let Some(input) = &mut prompt {
                    match key.code {
                        KeyCode::Esc => prompt = None,
                        KeyCode::Enter => match input.submit(&entries) {
                            Ok(choice) => return Ok(Some(choice)),
                            Err(error) => input.error = Some(error),
                        },
                        KeyCode::Backspace => {
                            input.name.pop();
                            input.error = None;
                        }
                        KeyCode::Char(character)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            input.name.push(character);
                            input.error = None;
                        }
                        _ => (),
                    }
                    continue;
                }
                if let Some(draft) = &mut search_draft {
                    match key.code {
                        KeyCode::Esc => search_draft = None,
                        KeyCode::Enter => {
                            query = std::mem::take(draft);
                            search_draft = None;
                        }
                        KeyCode::Backspace => {
                            draft.pop();
                            selection.select(Some(0));
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            draft.clear();
                            selection.select(Some(0));
                        }
                        KeyCode::Char(character)
                            if !key
                                .modifiers
                                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                        {
                            draft.push(character);
                            selection.select(Some(0));
                        }
                        _ => (),
                    }
                    continue;
                }
                status = None;
                match key.code {
                    KeyCode::Char('q') => return Ok(None),
                    KeyCode::Esc if !query.is_empty() => {
                        query.clear();
                        selection.select(Some(0));
                    }
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('/') => search_draft = Some(query.clone()),
                    KeyCode::Down | KeyCode::Char('j') => {
                        if let Some(selected) = selected {
                            selection.select(Some((selected + 1) % visible.len()));
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(selected) = selected {
                            selection.select(Some((selected + visible.len() - 1) % visible.len()));
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(selected) = selected {
                            return Ok(Some(entries[visible[selected]].clone()));
                        }
                    }
                    KeyCode::Char('n') => {
                        if let Some(selected) = selected {
                            prompt = Some(NamePrompt {
                                // The selected profile is the template, even when
                                // the cursor is on one of its existing instances.
                                profile: entries[visible[selected]].profile,
                                name: String::new(),
                                error: None,
                            });
                        }
                    }
                    KeyCode::Char('e') => {
                        if let Some(selected) = selected {
                            return Ok(Some(Choice {
                                profile: entries[visible[selected]].profile,
                                instance: new_ephemeral_id()?,
                                running: false,
                            }));
                        }
                    }
                    KeyCode::Char('s') => {
                        if let Some(selected) = selected {
                            let choice = &entries[visible[selected]];
                            let profile = &profiles[choice.profile];
                            if !request_stop(profile, &choice.instance)?.success() {
                                status = Some(format!(
                                    "Failed to request stop for {}/{}",
                                    profile.get("name")?,
                                    choice.instance
                                ));
                                continue;
                            }
                            status = Some(format!(
                                "Stopping {}/{}…",
                                profile.get("name")?,
                                choice.instance
                            ));
                            entries = choices(profiles)?;
                        }
                    }
                    KeyCode::Char('d') => {
                        if let Some(selected) = selected {
                            let choice = entries[visible[selected]].clone();
                            match prepare_deletion(&profiles[choice.profile], &choice.instance) {
                                Ok(saved) => {
                                    deletion = Some(DeletePrompt {
                                        choice,
                                        saved,
                                        error: None,
                                    })
                                }
                                Err(error) => status = Some(error.to_string()),
                            }
                        }
                    }
                    _ => (),
                }
            }
            // Avoid re-querying systemd for every frame or cursor movement.
            if prompt.is_none()
                && deletion.is_none()
                && refreshed.elapsed() >= Duration::from_secs(2)
            {
                entries = choices(profiles)?;
                refreshed = Instant::now();
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_indices_still_point_to_the_original_choices() {
        let profile = |name: &str| Profile {
            dir: Default::default(),
            values: [("name".into(), name.into())].into(),
            mounts: vec![],
            host_environment: vec![],
            session_rules: vec![],
            system_rules: vec![],
            pasta: vec![],
        };
        let profiles = [profile("dev"), profile("Steam")];
        let entries = [
            Choice {
                profile: 0,
                instance: "main".into(),
                running: true,
            },
            Choice {
                profile: 1,
                instance: "main".into(),
                running: false,
            },
            Choice {
                profile: 0,
                instance: "n-project".into(),
                running: false,
            },
        ];

        assert_eq!(matching_indices(&entries, &profiles, "STEAM"), [1]);
        assert_eq!(matching_indices(&entries, &profiles, "project"), [2]);
        assert_eq!(matching_indices(&entries, &profiles, "dev n-pro"), [2]);
        assert!(matching_indices(&entries, &profiles, "missing").is_empty());
    }

    #[test]
    fn named_instance_uses_the_selected_template_and_checks_duplicates() {
        let mut prompt = NamePrompt {
            profile: 1,
            name: "my-project".into(),
            error: None,
        };
        let existing = Choice {
            profile: 0,
            instance: "n-my-project".into(),
            running: false,
        };
        let choice = prompt.submit(&[existing]).unwrap();
        assert_eq!(choice.profile, 1);
        assert_eq!(choice.instance, "n-my-project");
        assert!(!choice.running);
        assert!(prompt.submit(&[choice]).is_err());

        for invalid in [
            "",
            "../escape",
            "two words",
            "Uppercase",
            "0123456789012345678901234",
        ] {
            prompt.name = invalid.into();
            assert!(prompt.submit(&[]).is_err(), "{invalid}");
        }
    }

    #[test]
    fn running_instances_have_a_green_indicator() {
        assert_eq!(running_indicator(true).content, "● ");
        assert!(
            running_indicator(true)
                .style
                .fg
                .is_some_and(|color| color == Color::Green)
        );
        assert_eq!(running_indicator(false).content, "  ");
        assert_eq!(running_indicator(false).style.fg, None);
    }
}
