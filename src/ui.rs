//! Pure layout: the same screen is rendered by Zellij and exercised by tests.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Style {
    Normal,
    Heading,
    Muted,
    Selected,
    Alert,
}
#[derive(Clone, Debug)]
pub(super) struct Line {
    pub text: String,
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub style: Style,
}
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Hit {
    Agent(usize),
    Inactive,
    Worktree(usize),
}
#[derive(Default)]
pub(super) struct Screen {
    pub lines: Vec<Line>,
    pub hits: BTreeMap<usize, Hit>,
}
impl Screen {
    fn put(&mut self, x: usize, y: usize, width: usize, text: impl AsRef<str>, style: Style) {
        if width > 0 {
            self.lines.push(Line {
                text: truncate(text.as_ref(), width),
                x,
                y,
                width,
                style,
            });
        }
    }
}

fn status(agent: &AgentRecord) -> &'static str {
    match agent.status.as_str() {
        "needs_input" => "Needs you",
        "error" => "Failed",
        "working" => "Working",
        "done" => "Done",
        "parked" => "Parked",
        "ended" => "Closed",
        _ => "Idle",
    }
}

fn age(now: u64, since: u64) -> String {
    if since == 0 {
        return String::new();
    }
    let seconds = now.saturating_sub(since);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h {}m", seconds / 3600, seconds % 3600 / 60)
    } else {
        format!("{}d", seconds / 86400)
    }
}

fn row_title(agent: &AgentRecord, width: usize, now: u64, nested: Option<&str>) -> String {
    let right = format!("{} · {}", status(agent), age(now, agent.status_since));
    let prefix = if agent.unread { "● " } else { "  " };
    let identity = if let Some(connector) = nested {
        format!("{connector} {}", agent.title)
    } else {
        format!("{}: {}", agent.project, agent.title)
    };
    if width < 44 {
        return truncate(
            &format!(
                "{prefix}{} {} {identity}",
                status_symbol(&agent.status),
                status(agent)
            ),
            width,
        );
    }
    let left_width = width.saturating_sub(right.width() + 2);
    let left = truncate(&format!("{prefix}{identity}"), left_width);
    format!(
        "{left}{}{right}",
        " ".repeat(width.saturating_sub(left.width() + right.width()))
    )
}

fn branch_label(agent: &AgentRecord) -> String {
    let branch = if agent.branch.is_empty() {
        "no branch"
    } else {
        &agent.branch
    };
    let dirty = if agent.dirty { " *" } else { "" };
    if agent.worktree.is_empty() || agent.worktree == "main" {
        format!("{branch}{dirty}")
    } else {
        format!("{branch}{dirty} · wt: {}", agent.worktree)
    }
}

fn row_context(agent: &AgentRecord, width: usize) -> String {
    let explanation = if !agent.activity.is_empty() {
        &agent.activity
    } else {
        &agent.message
    };
    let branch = truncate(&branch_label(agent), width / 2);
    if explanation.is_empty() {
        return format!("  {branch} · {}", agent.zellij_session);
    }
    let available = width.saturating_sub(branch.width() + 5);
    format!("  {} · {branch}", truncate(explanation, available))
}

fn group_title(agent: &AgentRecord, by_project: bool) -> String {
    if by_project {
        return format!("{}  ·  {}", agent.project, agent.repository_root);
    }
    match group_rank(agent) {
        0 => "NEEDS YOU",
        1 => "NEW RESULTS",
        2 => "WORKING",
        _ => "IDLE / SEEN / PARKED",
    }
    .into()
}

fn summary(deck: &AgentDeck) -> String {
    let live = deck
        .model
        .agents
        .iter()
        .filter(|a| deck.model.includes_kind(a) && is_attached(a));
    let mut needs = 0;
    let mut working = 0;
    let mut results = 0;
    for agent in live {
        match group_rank(agent) {
            0 => needs += 1,
            1 => results += 1,
            2 => working += 1,
            _ => {}
        }
    }
    format!("{needs} need you · {working} working · {results} new results")
}

fn agent_name(agent: &AgentRecord) -> &'static str {
    match agent.kind.as_str() {
        "opencode" => "OpenCode",
        "pi" => "Pi",
        _ => "Codex",
    }
}

fn details(agent: &AgentRecord, now: u64) -> Vec<String> {
    let mut lines = vec![
        agent.title.clone(),
        format!("{} · {}", status(agent), age(now, agent.status_since)),
        branch_label(agent),
        format!("Worktree  {}", agent.project_root),
        format!("Agent  {}", agent_name(agent)),
    ];
    if !agent.message.is_empty() {
        lines.insert(2, agent.message.clone());
    }
    if agent.cwd != agent.project_root {
        lines.push(format!("Directory  {}", agent.cwd));
    }
    lines.push(format!(
        "Session  {} · pane {}",
        agent.zellij_session,
        agent
            .pane_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "closed".into())
    ));
    if !agent.pr.is_empty() {
        lines.push(format!("PR  {}", agent.pr));
    }
    if !agent.ports.is_empty() {
        lines.push(format!(
            "Ports  {}",
            agent
                .ports
                .iter()
                .map(u16::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !agent.model.is_empty() {
        lines.push(format!("Model  {}", agent.model));
    }
    if !agent.activity.is_empty() {
        lines.push(format!(
            "{} · {}",
            agent.activity,
            age(now, agent.activity_since)
        ));
    }
    lines
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![];
    }
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.width() + word.width() + 1 > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        for ch in word.chars() {
            let size = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            if line.width() + size > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            if size <= width {
                line.push(ch);
            }
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn worktree_screen(deck: &AgentDeck, rows: usize, cols: usize) -> Screen {
    let mut screen = Screen::default();
    let margin = usize::from(cols > 2);
    let width = cols.saturating_sub(margin * 2);
    let repository = deck
        .action_target
        .as_ref()
        .map(|a| a.project.as_str())
        .unwrap_or("Repository");
    screen.put(
        margin,
        0,
        width,
        format!("Worktrees  /  {repository}"),
        Style::Heading,
    );
    if matches!(
        deck.mode,
        InputMode::WorktreePick | InputMode::WorktreeSearch
    ) {
        let query = if deck.mode == InputMode::WorktreeSearch {
            &deck.input
        } else {
            &deck.worktree_query
        };
        screen.put(
            margin,
            1,
            width,
            if query.is_empty() {
                "Enter opens an existing agent or offers to start one".into()
            } else {
                format!("Search: {query}")
            },
            Style::Muted,
        );
        let matching = deck.worktree_indices();
        let capacity = rows.saturating_sub(7) / 2;
        let scroll = deck
            .worktree_selected
            .saturating_sub(capacity.saturating_sub(1));
        for (offset, index) in matching.iter().skip(scroll).take(capacity).enumerate() {
            let item = &deck.worktrees[*index];
            let position = scroll + offset;
            let suffix = if item.prunable {
                "missing checkout".into()
            } else if item.agents.is_empty() {
                "start session".into()
            } else {
                format!(
                    "{} live {}",
                    item.agents.len(),
                    if item.agents.len() == 1 {
                        "agent"
                    } else {
                        "agents"
                    }
                )
            };
            let identity = format!(
                "{}{}{} · {suffix}",
                item.branch,
                if item.current { " (current)" } else { "" },
                if item.locked { " [locked]" } else { "" }
            );
            let selected = position == deck.worktree_selected;
            let style = if selected {
                Style::Selected
            } else {
                Style::Normal
            };
            let y = 3 + offset * 2;
            screen.put(margin, y, width, identity, style);
            screen.put(
                margin,
                y + 1,
                width,
                format!("  {}", item.path),
                if selected {
                    Style::Selected
                } else {
                    Style::Muted
                },
            );
            screen.hits.insert(y, Hit::Worktree(position));
            screen.hits.insert(y + 1, Hit::Worktree(position));
        }
        if matching.is_empty() && !deck.worktree_busy {
            screen.put(
                margin,
                3,
                width,
                "No matching worktrees · c clear search · n create",
                Style::Muted,
            );
        }
        if let Some(index) = matching.get(deck.worktree_selected) {
            screen.put(
                margin,
                rows - 4,
                width,
                &deck.worktrees[*index].path,
                Style::Muted,
            );
        }
        screen.put(margin, rows - 2, width, &deck.notice, Style::Normal);
        screen.put(
            margin,
            rows - 1,
            width,
            if deck.mode == InputMode::WorktreeSearch {
                "Type to search · Enter apply · Esc back"
            } else {
                "Enter open · n new worktree · / search · c clear · g refresh · Esc back"
            },
            Style::Muted,
        );
        return screen;
    }
    let mut lines = Vec::new();
    let agent_name = deck.action_target.as_ref().map_or("Codex", agent_name);
    if let Some(plan) = &deck.worktree_plan {
        lines.push(if plan.existing {
            format!("Start a new {agent_name} session")
        } else {
            format!("Create a new worktree and {agent_name} session")
        });
        lines.push(format!("Branch       {}", plan.branch));
        if !plan.existing {
            lines.push(format!(
                "Based on     {} @ {}",
                plan.base_branch,
                truncate(&plan.base_head, 12)
            ));
        }
        lines.push(format!("Destination  {}", plan.path));
        if deck.mode == InputMode::ConfirmWorktree {
            lines.push(format!(
                "First prompt {}",
                if deck.worktree_prompt.is_empty() {
                    "None — ready for input"
                } else {
                    &deck.worktree_prompt
                }
            ));
        }
    } else if let Some(agent) = &deck.action_target {
        lines.push(format!("New branch from {}", branch_label(agent)));
        lines.push(format!("Source  {}", agent.project_root));
    }
    if deck.mode == InputMode::WorktreeBranch {
        lines.push(format!("Branch name: {}_", deck.input));
    }
    if deck.mode == InputMode::WorktreePrompt {
        lines.push(format!("Optional first prompt: {}_", deck.input));
    }
    for (index, line) in lines
        .iter()
        .flat_map(|line| wrap(line, width))
        .take(rows.saturating_sub(6))
        .enumerate()
    {
        screen.put(margin, index + 2, width, line, Style::Normal);
    }
    for (index, line) in wrap(&deck.notice, width).into_iter().take(2).enumerate() {
        screen.put(margin, rows - 3 + index, width, line, Style::Heading);
    }
    screen.put(
        margin,
        rows - 1,
        width,
        if deck.worktree_busy {
            "Please wait…"
        } else if deck.mode == InputMode::ConfirmWorktree {
            "y confirm · n / Esc back"
        } else {
            "Enter continue · Esc cancel"
        },
        Style::Muted,
    );
    screen
}

pub(super) fn screen(deck: &mut AgentDeck, rows: usize, cols: usize, now: u64) -> Screen {
    let mut screen = Screen::default();
    if rows == 0 || cols == 0 {
        return screen;
    }
    let margin = usize::from(cols > 2);
    let width = cols.saturating_sub(margin * 2);
    if deck.compact_status {
        screen.put(
            margin,
            0,
            width,
            format!("Agents  {} · Alt a: deck", summary(deck)),
            Style::Heading,
        );
        return screen;
    }
    screen.put(
        margin,
        0,
        width,
        format!("Agent Deck   {}", summary(deck)),
        Style::Heading,
    );
    if rows < 7 {
        if rows > 1 {
            screen.put(
                margin,
                1,
                width,
                "Enlarge pane to browse · q close",
                Style::Muted,
            );
        }
        return screen;
    }
    if matches!(
        deck.mode,
        InputMode::WorktreePick
            | InputMode::WorktreeSearch
            | InputMode::WorktreeBranch
            | InputMode::WorktreePrompt
            | InputMode::ConfirmWorktree
    ) {
        return worktree_screen(deck, rows, cols);
    }
    if deck.mode == InputMode::Details {
        if let Some(agent) = deck.selected_agent() {
            let lines = details(&agent, now)
                .iter()
                .flat_map(|line| wrap(line, width))
                .collect::<Vec<_>>();
            deck.detail_scroll = deck.detail_scroll.min(lines.len().saturating_sub(rows - 4));
            for (index, line) in lines
                .iter()
                .skip(deck.detail_scroll)
                .take(rows - 4)
                .enumerate()
            {
                screen.put(margin, index + 2, width, line, Style::Normal);
            }
        }
        screen.put(
            margin,
            rows - 1,
            width,
            "↑/↓ scroll · PgUp/PgDn page · Tab / Esc back",
            Style::Muted,
        );
        return screen;
    }
    if matches!(
        deck.mode,
        InputMode::Reply | InputMode::ConfirmReply | InputMode::Title | InputMode::ConfirmPark
    ) {
        let heading = match deck.mode {
            InputMode::Reply | InputMode::ConfirmReply => "Reply to agent",
            InputMode::Title => "Set session title",
            _ => "Park session",
        };
        screen.put(margin, 2, width, heading, Style::Heading);
        let mut lines = Vec::new();
        if let Some(agent) = deck.selected_agent() {
            lines.push(format!("{}: {}", agent.project, agent.title));
            lines.push(format!(
                "{} · {}",
                branch_label(&agent),
                agent.zellij_session
            ));
            lines.push(agent.cwd);
        }
        if deck.mode == InputMode::ConfirmReply {
            lines.push(deck.staged.clone());
        } else if deck.mode == InputMode::ConfirmPark {
            lines.push("Send Ctrl-C to this agent's pane?".into());
        } else {
            lines.push(format!("{}_", deck.input));
        }
        for (index, line) in lines
            .iter()
            .flat_map(|line| wrap(line, width))
            .take(rows - 7)
            .enumerate()
        {
            screen.put(margin, index + 4, width, line, Style::Normal);
        }
        screen.put(margin, rows - 2, width, &deck.notice, Style::Heading);
        screen.put(
            margin,
            rows - 1,
            width,
            if matches!(deck.mode, InputMode::ConfirmReply | InputMode::ConfirmPark) {
                "y confirm · n / Esc cancel"
            } else {
                "Enter continue · Esc cancel"
            },
            Style::Muted,
        );
        return screen;
    }
    if deck.mode == InputMode::Help {
        let help = [
            "NAVIGATE  ↑/↓ or j/k select · Enter open · n next attention",
            "DETAILS  Tab full details · ↑/↓ or PgUp/PgDn scroll",
            "FIND  / search task, branch, worktree, path or session · c clear",
            "VIEWS  v attention / repository · i expand idle / seen / parked",
            "FILTERS  1 all · 2 unread · 3 working · 4 needs you",
            "         5 done · 6 parked · 7 resume closed sessions",
            "WORKTREES  w browse or create worktrees for selected repository",
            "ACTIONS  r reply · t title · m mark read · d dismiss",
            "         p park (confirm) · R resume · g refresh Git / PR / ports",
            "SUBAGENTS  s show / hide · unread is independent of parent",
            "READ STATE  visiting a pane marks the observed result read",
            "            approvals and failures stay until the agent continues",
        ];
        let lines = help.iter().flat_map(|line| wrap(line, width));
        for (index, line) in lines.take(rows.saturating_sub(3)).enumerate() {
            screen.put(margin, index + 2, width, line, Style::Normal);
        }
        screen.put(margin, rows - 1, width, "Esc / ? back", Style::Muted);
        return screen;
    }
    let filter_labels = [
        "All",
        "Unread",
        "Working",
        "Needs you",
        "Done",
        "Parked",
        "Resume",
    ];
    let filters = filter_labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            if deck.model.filter == i {
                format!("[{} {label}]", i + 1)
            } else {
                format!("{} {label}", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("  ");
    screen.put(margin, 1, width, filters, Style::Muted);
    let query = if deck.mode == InputMode::Search {
        &deck.input
    } else {
        &deck.model.query
    };
    let context = if let Some(path) = &deck.model.worktree_scope {
        format!("Worktree: {path} · c clear · Search: {query}")
    } else if query.is_empty() {
        format!(
            "{} view · v switch · subagents {}",
            if deck.model.group_by_project {
                "Repository"
            } else {
                "Attention"
            },
            if deck.model.show_subagents {
                "shown"
            } else {
                "hidden"
            }
        )
    } else {
        format!("Search: {query}")
    };
    screen.put(margin, 2, width, context, Style::Muted);

    let wide = cols >= 112 && rows >= 16;
    let detail_width = if wide {
        (width / 3).clamp(34, 52)
    } else {
        width
    };
    let list_width = if wide {
        width - detail_width - 3
    } else {
        width
    };
    let detail_height = if !wide && rows >= 20 { 5 } else { 0 };
    let bottom = rows.saturating_sub(3 + detail_height);
    let available = bottom.saturating_sub(3);
    let matching = deck.matching_indices();
    let mut logical: Vec<(String, Style, Option<Hit>)> = Vec::new();
    let mut previous_group = String::new();
    for (position, index) in matching.iter().enumerate() {
        let agent = &deck.model.agents[*index];
        let nested = subagent_connector(&deck.model.agents, &matching, position);
        let group = if nested.is_some() {
            previous_group.clone()
        } else {
            group_title(agent, deck.model.group_by_project)
        };
        if group != previous_group {
            logical.push((group.clone(), Style::Heading, None));
            previous_group = group;
        }
        let selected = position == deck.model.selected;
        let style = if selected {
            Style::Selected
        } else if needs_attention(agent) {
            Style::Alert
        } else {
            Style::Normal
        };
        logical.push((
            row_title(agent, list_width, now, nested),
            style,
            Some(Hit::Agent(position)),
        ));
        logical.push((
            row_context(agent, list_width),
            if selected {
                Style::Selected
            } else {
                Style::Muted
            },
            Some(Hit::Agent(position)),
        ));
    }
    let hidden = deck
        .model
        .agents
        .iter()
        .filter(|agent| {
            deck.model.includes_kind(agent) && is_attached(agent) && group_rank(agent) == 3
        })
        .count();
    if deck.model.filter == 0 && query.is_empty() && !deck.model.show_inactive && hidden > 0 {
        logical.push((
            format!("▸ {hidden} idle / seen / parked · i expand"),
            Style::Muted,
            Some(Hit::Inactive),
        ));
    }
    if logical.is_empty() {
        logical.push((
            if query.is_empty() {
                "No sessions in this view"
            } else {
                "No matching sessions"
            }
            .into(),
            Style::Muted,
            None,
        ));
        logical.push((
            "1 all · 7 resume · / search · c clear search".into(),
            Style::Muted,
            None,
        ));
    }
    let selected_line = logical
        .iter()
        .position(|(_, _, hit)| *hit == Some(Hit::Agent(deck.model.selected)))
        .unwrap_or(0);
    let selected_end = (selected_line + 2).min(logical.len());
    let mut scroll = deck
        .model
        .viewport_start
        .min(logical.len().saturating_sub(available));
    if selected_line < scroll {
        scroll = selected_line.saturating_sub(1);
    }
    if selected_end > scroll + available {
        scroll = selected_end.saturating_sub(available);
    }
    deck.model.viewport_start = scroll;
    for (offset, (text, style, hit)) in logical.iter().skip(scroll).take(available).enumerate() {
        let y = 3 + offset;
        screen.put(margin, y, list_width, text, *style);
        if let Some(hit) = hit {
            screen.hits.insert(y, hit.clone());
        }
    }
    if let Some(agent) = deck.selected_agent() {
        if wide || detail_height > 0 {
            let x = if wide {
                margin + list_width + 3
            } else {
                margin
            };
            let y = if wide { 3 } else { bottom + 1 };
            let height = if wide {
                rows.saturating_sub(y + 3)
            } else {
                detail_height - 1
            };
            let detail = if wide {
                details(&agent, now)
            } else {
                vec![
                    format!(
                        "{} · {} · {}",
                        branch_label(&agent),
                        agent.pr,
                        agent.zellij_session
                    ),
                    agent.cwd.clone(),
                    agent.message.clone(),
                ]
            };
            for (index, line) in detail
                .iter()
                .flat_map(|line| wrap(line, detail_width))
                .take(height)
                .enumerate()
            {
                screen.put(
                    x,
                    y + index,
                    detail_width,
                    line,
                    if index == 0 {
                        Style::Heading
                    } else {
                        Style::Muted
                    },
                );
            }
        }
    }
    let prompt = if matches!(
        deck.mode,
        InputMode::Browse | InputMode::ConfirmReply | InputMode::ConfirmPark
    ) {
        deck.notice.clone()
    } else {
        format!("{}: {}_", deck.notice, deck.input)
    };
    screen.put(margin, rows - 2, width, prompt, Style::Normal);
    screen.put(
        margin,
        rows - 1,
        width,
        "Enter open · Tab details · n next · w worktrees · / search · ? help",
        Style::Muted,
    );
    screen
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pi_details_and_worktree_confirmation_identify_the_agent() {
        let agent = AgentRecord {
            kind: "pi".into(),
            ..Default::default()
        };
        assert!(details(&agent, 100).contains(&"Agent  Pi".to_owned()));
        let mut deck = AgentDeck {
            mode: InputMode::ConfirmWorktree,
            action_target: Some(agent),
            worktree_plan: Some(crate::WorktreePlan {
                existing: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let output = screen(&mut deck, 24, 100, 100);
        assert!(output
            .lines
            .iter()
            .any(|line| line.text.contains("new Pi session")));
    }

    #[test]
    fn layout_fits_terminal_cells_and_clicks_respect_multiline_rows() {
        let mut deck = AgentDeck::default();
        deck.model.agents = (0..10)
            .map(|i| AgentRecord {
                key: format!("agent:{i}"),
                project: "工具".into(),
                title: "Fix search".into(),
                status: "working".into(),
                zellij_session: "test".into(),
                pane_id: Some(i),
                branch: "feature/search".into(),
                activity: "Running tests".into(),
                ..Default::default()
            })
            .collect();
        deck.model.selected = 7;
        for (rows, cols) in [(0, 0), (1, 1), (6, 20), (12, 40), (24, 80), (35, 140)] {
            let screen = screen(&mut deck, rows, cols, 100);
            for line in &screen.lines {
                assert!(line.y < rows);
                assert!(line.x + line.text.width() <= cols, "{:?}", line);
            }
            if rows >= 7 {
                let selected: Vec<_> = screen
                    .hits
                    .iter()
                    .filter(|(_, hit)| **hit == Hit::Agent(7))
                    .collect();
                assert_eq!(selected.len(), 2);
                assert_eq!(selected[1].0 - selected[0].0, 1);
            }
        }
    }

    #[test]
    fn waiting_seen_stays_above_new_results_and_idle_is_collapsed() {
        let mut deck = AgentDeck::default();
        deck.model.agents = ["idle", "done", "needs_input", "working"]
            .iter()
            .enumerate()
            .map(|(i, status)| AgentRecord {
                key: format!("{i}"),
                status: status.to_string(),
                unread: *status == "done",
                zellij_session: "test".into(),
                pane_id: Some(i as u32),
                ..Default::default()
            })
            .collect();
        assert_eq!(deck.matching_indices(), vec![2, 1, 3]);
        let output = screen(&mut deck, 30, 100, 100)
            .lines
            .iter()
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(output.contains("NEEDS YOU"));
        assert!(output.contains("1 idle / seen / parked"));
    }

    #[test]
    fn unicode_truncation_uses_display_width() {
        assert_eq!(truncate("工具abcdef", 5), "工具…");
        assert_eq!(truncate("anything", 0), "");
        assert_eq!(age(100, 200), "0s");
        assert_eq!(age(100, 0), "");
    }

    #[test]
    fn worktree_picker_scrolling_and_confirmation_fit_small_panes() {
        let mut deck = AgentDeck {
            mode: InputMode::WorktreePick,
            worktrees: (0..12)
                .map(|i| WorktreeInfo {
                    branch: format!("feature/{i}"),
                    path: format!("/example/repo/worktree-{i}"),
                    ..Default::default()
                })
                .collect(),
            worktree_selected: 8,
            ..Default::default()
        };
        let output = screen(&mut deck, 18, 55, 100);
        assert_eq!(
            output
                .hits
                .values()
                .filter(|hit| **hit == Hit::Worktree(8))
                .count(),
            2
        );
        deck.open_selected_worktree();
        let output = screen(&mut deck, 18, 55, 100);
        let text = output
            .lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("/example/repo/worktree-8"));
        assert!(text.contains("y confirm"));
        assert!(output
            .lines
            .iter()
            .all(|line| line.y < 18 && line.x + line.text.width() <= 55));
    }

    #[test]
    fn reply_confirmation_shows_the_actual_message_and_target() {
        let mut deck = AgentDeck {
            mode: InputMode::ConfirmReply,
            staged: "Please review the search changes".into(),
            action_target: Some(AgentRecord {
                project: "shop".into(),
                title: "Search".into(),
                branch: "feature/search".into(),
                cwd: "/example/search".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let output = screen(&mut deck, 24, 80, 100);
        let text = output
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Please review the search changes"));
        assert!(text.contains("feature/search"));
        assert!(text.contains("y confirm"));
    }

    #[test]
    fn compact_status_fits_a_single_row() {
        let mut deck = AgentDeck {
            compact_status: true,
            ..Default::default()
        };
        let output = screen(&mut deck, 1, 80, 100);
        assert_eq!(output.lines.len(), 1);
        assert_eq!(output.lines[0].y, 0);
        assert!(output.lines[0].text.contains("0 need you"));
        assert!(output.hits.is_empty());
    }
}
