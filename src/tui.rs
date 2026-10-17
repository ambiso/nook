use crate::{Choice, Profile, Result, choices, control_instance, new_ephemeral_id, parse_name};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
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
        loop {
            if entries.is_empty() {
                return Err("no sandbox profiles are configured".into());
            }
            let selected = selection.selected().unwrap_or(0).min(entries.len() - 1);
            selection.select(Some(selected));
            terminal.draw(|frame| {
                let [list_area, help_area] =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(2)])
                        .areas(frame.area());
                let rows = entries.iter().map(|choice| {
                    ListItem::new(format!(
                        "{:<24} {:<22} {}",
                        profiles[choice.profile].values["name"],
                        choice.instance,
                        if choice.running { "running" } else { "stopped" },
                    ))
                });
                let list = List::new(rows)
                    .block(Block::bordered().title(" Nook "))
                    .highlight_symbol("› ")
                    .highlight_style(Style::new().fg(Color::Cyan));
                frame.render_stateful_widget(list, list_area, &mut selection);
                frame.render_widget(
                    Paragraph::new(
                        "↑/↓ j/k: select   Enter: launch/enter   n: named   e: ephemeral   s: stop   q: quit",
                    )
                    .wrap(Wrap { trim: false }),
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
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(None),
                    KeyCode::Down | KeyCode::Char('j') => {
                        selection.select(Some((selected + 1) % entries.len()))
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        selection.select(Some((selected + entries.len() - 1) % entries.len()))
                    }
                    KeyCode::Enter => return Ok(Some(entries[selected].clone())),
                    KeyCode::Char('n') => {
                        prompt = Some(NamePrompt {
                            // The selected profile is the template, even when
                            // the cursor is on one of its existing instances.
                            profile: entries[selected].profile,
                            name: String::new(),
                            error: None,
                        });
                    }
                    KeyCode::Char('e') => {
                        return Ok(Some(Choice {
                            profile: entries[selected].profile,
                            instance: new_ephemeral_id()?,
                            running: false,
                        }));
                    }
                    KeyCode::Char('s') => {
                        let choice = &entries[selected];
                        control_instance(&profiles[choice.profile], &choice.instance, "stop")?;
                        entries = choices(profiles)?;
                    }
                    _ => (),
                }
            }
            // Avoid re-querying systemd for every frame or cursor movement.
            if prompt.is_none() && refreshed.elapsed() >= Duration::from_secs(2) {
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
}
