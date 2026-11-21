use crate::{
    Choice, Profile, Result, choices, delete_instance, new_ephemeral_id, parse_name,
    prepare_deletion, request_stop,
    storage::SavedInstance,
    tui_colors::ProfilePalette,
    usage::{Usage, UsageMonitor},
};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    borrow::Cow,
    io::{self, IsTerminal},
    ops::Range,
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

struct Columns {
    name: usize,
    stats_count: usize,
}

impl Columns {
    const STAT_WIDTH: usize = 10;

    fn fit(
        width: usize,
        profiles: &[Profile],
        entries: &[Choice],
        visible: &[VisibleRow<'_>],
    ) -> Self {
        let name = entries
            .iter()
            .map(|entry| {
                if entry.instance == "main" {
                    profiles[entry.profile].values["name"].len()
                } else {
                    Branch::Child.prefix().chars().count() + entry.instance.len()
                }
            })
            .chain(
                visible
                    .iter()
                    .map(|row| row.branch.prefix().chars().count() + row.label.len()),
            )
            .max()
            .unwrap_or(0)
            .max("Sandbox".len());
        // Reserve two cells each for the selection marker and the running dot.
        let base_width = 4 + name + 1 + "running".len();
        let stats_count = width.saturating_sub(base_width) / (1 + Self::STAT_WIDTH);
        Self { name, stats_count }
    }

    fn stats(&self, ram: &str, swap: &str, uptime: &str, cpu: &str) -> String {
        let mut text = String::new();
        let mut append = |value| {
            use std::fmt::Write;
            let _ = write!(text, " {value:>width$}", width = Self::STAT_WIDTH);
        };
        if self.stats_count >= 3 {
            append(uptime);
        }
        if self.stats_count >= 4 {
            append(cpu);
        }
        if self.stats_count >= 2 {
            append(ram);
            append(swap);
        }
        text
    }
}

fn format_bytes(bytes: Option<u64>) -> String {
    let Some(bytes) = bytes else {
        return "—".into();
    };
    let mut value = bytes as f64;
    for unit in ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"] {
        if value < 1024.0 || unit == "EiB" {
            return if unit == "B" {
                format!("{bytes} B")
            } else {
                format!("{value:.1} {unit}")
            };
        }
        value /= 1024.0;
    }
    unreachable!()
}

fn format_uptime(uptime: Option<u64>) -> String {
    let Some(seconds) = uptime else {
        return "—".into();
    };
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        3600..86400 => format!("{}h {}m", seconds / 3600, seconds / 60 % 60),
        _ => format!("{}d {}h", seconds / 86400, seconds / 3600 % 24),
    }
}

fn refresh(profiles: &[Profile], monitor: &mut UsageMonitor) -> Result<(Vec<Choice>, Vec<Usage>)> {
    let entries = choices(profiles)?;
    let usage = monitor.sample(profiles, &entries);
    Ok((entries, usage))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Branch {
    Profile,
    Child,
    LastChild,
}

impl Branch {
    fn prefix(self) -> &'static str {
        match self {
            Self::Profile => "",
            Self::Child => "  ├─ ",
            Self::LastChild => "  └─ ",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct VisibleRow<'a> {
    entry: usize,
    branch: Branch,
    matches: bool,
    label: Cow<'a, str>,
    highlights: Vec<Range<usize>>,
}

fn visible_rows<'a>(
    entries: &'a [Choice],
    profiles: &'a [Profile],
    query: &str,
) -> Vec<VisibleRow<'a>> {
    let query = query.to_ascii_lowercase();
    let mut rows = Vec::new();
    let mut first = 0;
    // The catalog lists each profile's main instance first, followed by its children.
    for group in entries.chunk_by(|left, right| left.profile == right.profile) {
        let name = profiles[group[0].profile].values["name"].as_str();
        // Search the full identity, then project matching byte ranges onto the
        // two labels the tree actually displays. Catalog names and IDs are ASCII.
        let hits = |instance: &str| -> Vec<Range<usize>> {
            if query.is_empty() {
                return Vec::new();
            }
            format!("{name} {instance}")
                .to_ascii_lowercase()
                .match_indices(&query)
                .map(|(start, found)| start..start + found.len())
                .collect()
        };
        let mut parent_hits = hits(&group[0].instance);
        let parent_matches = query.is_empty() || !parent_hits.is_empty();
        // The main instance ID is normally implicit. Show it during a search
        // that matched it so the highlighted text explains the result.
        let parent_label = if parent_hits.iter().any(|hit| hit.end > name.len()) {
            Cow::Owned(format!("{name} {}", group[0].instance))
        } else {
            Cow::Borrowed(name)
        };
        let parent = rows.len();
        rows.push(VisibleRow {
            entry: first,
            branch: Branch::Profile,
            matches: parent_matches,
            label: parent_label,
            highlights: Vec::new(),
        });
        for (index, child) in group.iter().enumerate().skip(1) {
            let child_hits = hits(&child.instance);
            if query.is_empty() || !child_hits.is_empty() {
                parent_hits.extend(
                    child_hits
                        .iter()
                        .filter_map(|hit| highlight_in(hit, 0..name.len())),
                );
                let start = name.len() + 1;
                rows.push(VisibleRow {
                    entry: first + index,
                    branch: Branch::Child,
                    matches: true,
                    label: Cow::Borrowed(&child.instance),
                    highlights: child_hits
                        .iter()
                        .filter_map(|hit| highlight_in(hit, start..start + child.instance.len()))
                        .collect(),
                });
            }
        }
        if rows.len() > parent + 1 {
            rows.last_mut().unwrap().branch = Branch::LastChild;
        } else if !parent_matches {
            rows.pop();
        }
        if let Some(row) = rows.get_mut(parent) {
            parent_hits.sort_unstable_by_key(|range| range.start);
            row.highlights = parent_hits;
        }
        first += group.len();
    }
    rows
}

fn highlight_in(hit: &Range<usize>, label: Range<usize>) -> Option<Range<usize>> {
    let start = hit.start.max(label.start);
    let end = hit.end.min(label.end);
    (start < end).then(|| start - label.start..end - label.start)
}

fn name_spans<'a>(row: &'a VisibleRow<'_>, width: usize, color: Color) -> Vec<Span<'a>> {
    let style = Style::new().fg(color);
    let highlight = style
        .add_modifier(Modifier::UNDERLINED)
        .remove_modifier(Modifier::DIM);
    let label = row.label.as_ref();
    let mut spans = Vec::new();
    let mut cursor = 0;
    for hit in &row.highlights {
        // Several children can match the same piece of their shared profile name.
        if hit.end <= cursor {
            continue;
        }
        if cursor < hit.start {
            spans.push(Span::styled(&label[cursor..hit.start], style));
        }
        spans.push(Span::styled(
            &label[cursor.max(hit.start)..hit.end],
            highlight,
        ));
        cursor = hit.end;
    }
    spans.push(Span::styled(
        format!("{:<width$}", &label[cursor..], width = width - cursor),
        style,
    ));
    spans
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
        let palette = ProfilePalette::detect();
        let profile_colors: Vec<_> = profiles
            .iter()
            .map(|profile| palette.color(&profile.values["name"]))
            .collect();
        let mut monitor = UsageMonitor::default();
        let (mut entries, mut usage) = refresh(profiles, &mut monitor)?;
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
            let visible = visible_rows(&entries, profiles, active_query);
            // Context rows show where a match lives but never receive launch or stop actions.
            let selectable: Vec<_> = visible
                .iter()
                .enumerate()
                .filter_map(|(index, row)| row.matches.then_some(index))
                .collect();
            let cursor = selection
                .selected()
                .and_then(|selected| selectable.iter().position(|&index| index == selected))
                .or_else(|| (!selectable.is_empty()).then_some(0));
            let selected = cursor.map(|index| selectable[index]);
            selection.select(selected);
            let selected_choice = selected.map(|index| &entries[visible[index].entry]);
            terminal.draw(|frame| {
                let [list_area, help_area] =
                    Layout::vertical([Constraint::Min(3), Constraint::Length(3)])
                        .areas(frame.area());
                let border = Block::bordered().title(format!(" Nook ({}/{}) ", selectable.len(), entries.len()));
                let [header_area, rows_area] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)])
                    .areas(border.inner(list_area));
                frame.render_widget(border, list_area);
                let columns = Columns::fit(header_area.width.into(), profiles, &entries, &visible);
                let header = format!(
                    "    {:<width$} {:<7}{}",
                    "Sandbox",
                    "Status",
                    columns.stats("RAM", "Swap", "Uptime", "CPU"),
                    width = columns.name,
                );
                frame.render_widget(Line::styled(header, Style::new().add_modifier(Modifier::BOLD)), header_area);

                let rows = visible.iter().map(|row| {
                    let choice = &entries[row.entry];
                    let usage = &usage[row.entry];
                    let prefix = row.branch.prefix();
                    let name_width = columns.name - prefix.chars().count();
                    let status = if choice.running { "running" } else { "stopped" };
                    let cpu = usage.cpu_percent.map_or_else(
                        || "—".into(),
                        |percent| format!("{percent:.1}%"),
                    );
                    let metrics = columns.stats(
                        &format_bytes(usage.ram_bytes),
                        &format_bytes(usage.swap_bytes),
                        &format_uptime(usage.uptime_seconds),
                        &cpu,
                    );
                    let mut spans = vec![
                        Span::raw(prefix),
                        running_indicator(choice.running),
                    ];
                    spans.extend(name_spans(row, name_width, profile_colors[choice.profile]));
                    spans.push(Span::raw(format!(" {status:<7}{metrics}")));
                    let line = Line::from(spans);
                    if row.matches {
                        ListItem::new(line)
                    } else {
                        ListItem::new(line).style(Style::new().add_modifier(Modifier::DIM))
                    }
                });
                let list = List::new(rows)
                    .highlight_symbol("› ")
                    .highlight_spacing(HighlightSpacing::Always)
                    .highlight_style(Style::new().add_modifier(Modifier::BOLD));
                frame.render_stateful_widget(list, rows_area, &mut selection);
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
                                    (entries, usage) = refresh(profiles, &mut monitor)?;
                                    refreshed = Instant::now();
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
                            if draft.pop().is_none() {
                                query.clear();
                                search_draft = None;
                            }
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
                        if let Some(cursor) = cursor {
                            selection.select(Some(selectable[(cursor + 1) % selectable.len()]));
                        }
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        if let Some(cursor) = cursor {
                            let previous = (cursor + selectable.len() - 1) % selectable.len();
                            selection.select(Some(selectable[previous]));
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(choice) = selected_choice {
                            return Ok(Some(choice.clone()));
                        }
                    }
                    KeyCode::Char('n') => {
                        if let Some(choice) = selected_choice {
                            prompt = Some(NamePrompt {
                                // The selected profile is the template, even when
                                // the cursor is on one of its existing instances.
                                profile: choice.profile,
                                name: String::new(),
                                error: None,
                            });
                        }
                    }
                    KeyCode::Char('e') => {
                        if let Some(choice) = selected_choice {
                            return Ok(Some(Choice {
                                profile: choice.profile,
                                instance: new_ephemeral_id()?,
                                running: false,
                            }));
                        }
                    }
                    KeyCode::Char('s') => {
                        if let Some(choice) = selected_choice {
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
                            (entries, usage) = refresh(profiles, &mut monitor)?;
                            refreshed = Instant::now();
                        }
                    }
                    KeyCode::Char('d') => {
                        if let Some(choice) = selected_choice {
                            match prepare_deletion(&profiles[choice.profile], &choice.instance) {
                                Ok(saved) => {
                                    deletion = Some(DeletePrompt {
                                        choice: choice.clone(),
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
                (entries, usage) = refresh(profiles, &mut monitor)?;
                refreshed = Instant::now();
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_search_keeps_a_nonselectable_parent_for_context() {
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
                profile: 0,
                instance: "n-project".into(),
                running: false,
            },
            Choice {
                profile: 1,
                instance: "main".into(),
                running: false,
            },
        ];

        let steam = visible_rows(&entries, &profiles, "STEAM");
        assert_eq!(steam[0].entry, 2);
        assert_eq!(steam[0].highlights, vec![0..5]);
        for (query, parent_hits, child_hit) in
            [("project", vec![], 2..9), ("dev n-pro", vec![0..3], 0..5)]
        {
            assert_eq!(
                visible_rows(&entries, &profiles, query),
                [
                    VisibleRow {
                        entry: 0,
                        branch: Branch::Profile,
                        matches: false,
                        label: Cow::Borrowed("dev"),
                        highlights: parent_hits,
                    },
                    VisibleRow {
                        entry: 1,
                        branch: Branch::LastChild,
                        matches: true,
                        label: Cow::Borrowed("n-project"),
                        highlights: vec![child_hit],
                    },
                ]
            );
        }
        let mains = visible_rows(&entries, &profiles, "main");
        assert_eq!(mains[0].label, "dev main");
        assert_eq!(mains[0].highlights, vec![4..8]);
        assert!(visible_rows(&entries, &profiles, "missing").is_empty());
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
