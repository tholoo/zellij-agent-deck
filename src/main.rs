use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use zellij_tile::prelude::*;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
struct AgentRecord {
    key: String,
    kind: String,
    codex_session_id: String,
    parent_key: String,
    zellij_session: String,
    pane_id: Option<u32>,
    cwd: String,
    project: String,
    project_root: String,
    title: String,
    status: String,
    unread: bool,
    message: String,
    model: String,
    branch: String,
    dirty: bool,
    pr: String,
    ports: Vec<u16>,
    updated_at: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
enum InputMode {
    #[default]
    Browse,
    Search,
    Reply,
    ConfirmReply,
    Title,
    WorktreeBranch,
    WorktreePrompt,
    ConfirmPark,
}

#[derive(Clone, Debug, PartialEq)]
enum JumpAction {
    MarkRead { key: String },
    HideDeck,
    FocusTerminalPane { pane_id: u32 },
    SwitchSession { session: String, pane_id: u32 },
}

fn jump_plan(current_session: &str, agent: &AgentRecord) -> Result<Vec<JumpAction>, &'static str> {
    let pane_id = agent
        .pane_id
        .ok_or("This agent no longer has a live pane; press R to resume it")?;
    if agent.zellij_session.is_empty() {
        return Err("This agent no longer has a live pane; press R to resume it");
    }
    let navigate = if agent.zellij_session == current_session {
        JumpAction::FocusTerminalPane { pane_id }
    } else {
        JumpAction::SwitchSession {
            session: agent.zellij_session.clone(),
            pane_id,
        }
    };
    Ok(vec![
        JumpAction::MarkRead {
            key: agent.key.clone(),
        },
        JumpAction::HideDeck,
        navigate,
    ])
}

#[derive(Default)]
struct AgentDeck {
    helper: String,
    current_session: String,
    agents: Vec<AgentRecord>,
    selected: usize,
    filter: usize,
    mode: InputMode,
    input: String,
    staged: String,
    notice: String,
    visible: bool,
    refresh_ticks: u8,
    next_list_request: u64,
    applied_list_request: u64,
}

const FILTERS: [&str; 6] = ["all", "unread", "running", "waiting", "done", "parked"];

impl AgentDeck {
    fn context(operation: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("operation".to_owned(), operation.to_owned())])
    }

    fn run_helper(&self, operation: &str, args: &[String]) {
        self.run_helper_with_context(args, Self::context(operation));
    }

    fn run_helper_with_context(&self, args: &[String], context: BTreeMap<String, String>) {
        let mut command = vec![self.helper.clone()];
        command.extend(args.iter().cloned());
        let refs = command.iter().map(String::as_str).collect::<Vec<_>>();
        run_command(&refs, context);
    }

    fn refresh(&mut self, enrich: bool) {
        let mut args = vec!["list".to_owned()];
        if enrich {
            args.push("--refresh".to_owned());
        }
        self.next_list_request = self.next_list_request.wrapping_add(1);
        let mut context = Self::context("list");
        context.insert("request_id".into(), self.next_list_request.to_string());
        self.run_helper_with_context(&args, context);
    }

    fn matches_filter(&self, agent: &AgentRecord) -> bool {
        match self.filter {
            1 => agent.unread,
            2 => agent.status == "working" || agent.status == "idle",
            3 => agent.status == "needs_input",
            4 => agent.status == "done" || agent.status == "ended",
            5 => agent.status == "parked",
            _ => true,
        }
    }

    fn matching_indices(&self) -> Vec<usize> {
        let query = if self.mode == InputMode::Search {
            self.input.to_lowercase()
        } else {
            self.staged
                .strip_prefix("search:")
                .unwrap_or("")
                .to_lowercase()
        };
        self.agents
            .iter()
            .enumerate()
            .filter(|(_, agent)| {
                self.matches_filter(agent)
                    && (query.is_empty()
                        || format!(
                            "{} {} {} {}",
                            agent.project, agent.title, agent.branch, agent.message
                        )
                        .to_lowercase()
                        .contains(&query))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_agent(&self) -> Option<AgentRecord> {
        let matches = self.matching_indices();
        matches
            .get(self.selected)
            .and_then(|index| self.agents.get(*index))
            .cloned()
    }

    fn clamp_selection(&mut self) {
        let len = self.matching_indices().len();
        self.selected = self.selected.min(len.saturating_sub(1));
    }

    fn move_selection(&mut self, delta: isize) {
        let len = self.matching_indices().len();
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = (self.selected as isize + delta).rem_euclid(len as isize) as usize;
        }
    }

    fn set_input_mode(&mut self, mode: InputMode, prompt: &str) {
        self.mode = mode;
        self.input.clear();
        self.notice = prompt.to_owned();
    }

    fn cancel_input(&mut self) {
        self.mode = InputMode::Browse;
        self.input.clear();
        self.staged.clear();
        self.notice.clear();
    }

    fn mutate_selected(&mut self, command: &str, extra: &[String]) {
        if let Some(agent) = self.selected_agent() {
            let mut args = vec![command.to_owned(), agent.key];
            args.extend(extra.iter().cloned());
            self.run_helper(command, &args);
            self.notice = format!("{} requested", command);
        }
    }

    fn jump_selected(&mut self) {
        if let Some(agent) = self.selected_agent() {
            match jump_plan(&self.current_session, &agent) {
                Ok(actions) => {
                    for action in actions {
                        match action {
                            JumpAction::MarkRead { key } => {
                                self.run_helper("mark-read", &["mark-read".into(), key]);
                            }
                            JumpAction::HideDeck => hide_self(),
                            JumpAction::FocusTerminalPane { pane_id } => {
                                focus_terminal_pane(pane_id, false, false);
                            }
                            JumpAction::SwitchSession { session, pane_id } => {
                                switch_session_with_focus(&session, None, Some((pane_id, false)));
                            }
                        }
                    }
                }
                Err(message) => self.notice = message.into(),
            }
        }
    }

    fn apply_agent_signal(&mut self, agent: AgentRecord) {
        if let Some(existing) = self
            .agents
            .iter_mut()
            .find(|existing| existing.key == agent.key)
        {
            *existing = agent.clone();
        } else {
            self.agents.push(agent.clone());
        }
        self.clamp_selection();
        if let Some(pane_id) = agent.pane_id {
            let pane = PaneId::Terminal(pane_id);
            let wants_attention =
                agent.unread && matches!(agent.status.as_str(), "needs_input" | "done");
            if wants_attention {
                highlight_and_unhighlight_panes(vec![pane], vec![]);
            } else {
                highlight_and_unhighlight_panes(vec![], vec![pane]);
            }
            let label = truncate(&format!("{}: {}", agent.project, agent.title), 80);
            rename_terminal_pane(pane_id, label);
        }
        self.refresh(false);
        self.applied_list_request = self.next_list_request;
    }

    fn submit_input(&mut self) {
        let value = self.input.trim().to_owned();
        match self.mode {
            InputMode::Search => {
                self.staged = format!("search:{value}");
                self.mode = InputMode::Browse;
                self.notice = if value.is_empty() {
                    String::new()
                } else {
                    format!("filter: {value}")
                };
                self.selected = 0;
            }
            InputMode::Reply if !value.is_empty() => {
                self.staged = value;
                self.mode = InputMode::ConfirmReply;
                self.notice = "Send this reply? y/n".into();
            }
            InputMode::Title if !value.is_empty() => {
                self.mutate_selected("title", &[value]);
                self.cancel_input();
            }
            InputMode::WorktreeBranch if !value.is_empty() => {
                self.staged = value;
                self.set_input_mode(
                    InputMode::WorktreePrompt,
                    "Optional first Codex prompt (Enter to skip)",
                );
            }
            InputMode::WorktreePrompt => {
                if let Some(agent) = self.selected_agent() {
                    self.run_helper(
                        "worktree",
                        &["worktree".into(), agent.key, self.staged.clone(), value],
                    );
                    self.cancel_input();
                    self.notice = "Creating worktree and Codex pane…".into();
                }
            }
            _ => {}
        }
        self.input.clear();
    }

    fn handle_key(&mut self, key: KeyWithModifier) {
        let bare = key.bare_key;
        match self.mode {
            InputMode::ConfirmReply => match bare {
                BareKey::Char('y') | BareKey::Char('Y') => {
                    let message = self.staged.clone();
                    self.mutate_selected("reply", &[message]);
                    self.cancel_input();
                }
                BareKey::Char('n') | BareKey::Char('N') | BareKey::Esc => self.cancel_input(),
                _ => {}
            },
            InputMode::ConfirmPark => match bare {
                BareKey::Char('y') | BareKey::Char('Y') => {
                    self.mutate_selected("park", &[]);
                    self.cancel_input();
                }
                BareKey::Char('n') | BareKey::Char('N') | BareKey::Esc => self.cancel_input(),
                _ => {}
            },
            InputMode::Browse => match bare {
                BareKey::Esc | BareKey::Char('q') => hide_self(),
                BareKey::Down | BareKey::Char('j') => self.move_selection(1),
                BareKey::Up | BareKey::Char('k') => self.move_selection(-1),
                BareKey::Enter => self.jump_selected(),
                BareKey::Char('/') => self.set_input_mode(InputMode::Search, "Search agents"),
                BareKey::Char('r') => {
                    self.set_input_mode(InputMode::Reply, "Reply to selected agent")
                }
                BareKey::Char('t') => self.set_input_mode(InputMode::Title, "Set task title"),
                BareKey::Char('w') => {
                    self.set_input_mode(InputMode::WorktreeBranch, "New worktree branch")
                }
                BareKey::Char('p') => {
                    self.mode = InputMode::ConfirmPark;
                    self.notice = "Park selected agent with Ctrl-C? y/n".into();
                }
                BareKey::Char('R') => self.mutate_selected("resume", &[]),
                BareKey::Char('m') => self.mutate_selected("mark-read", &[]),
                BareKey::Char('d') => self.mutate_selected("dismiss", &[]),
                BareKey::Char('g') => {
                    self.notice = "Refreshing git, PR, and port metadata…".into();
                    self.refresh(true);
                }
                BareKey::Char('c') => {
                    self.staged.clear();
                    self.notice.clear();
                }
                BareKey::Char(ch @ '1'..='6') => {
                    self.filter = ch as usize - '1' as usize;
                    self.selected = 0;
                }
                _ => {}
            },
            _ => match bare {
                BareKey::Esc => self.cancel_input(),
                BareKey::Enter => self.submit_input(),
                BareKey::Backspace => {
                    self.input.pop();
                }
                BareKey::Char(ch) if !key.key_modifiers.contains(&KeyModifier::Ctrl) => {
                    self.input.push(ch)
                }
                _ => {}
            },
        }
    }

    fn handle_result(
        &mut self,
        code: Option<i32>,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        context: BTreeMap<String, String>,
    ) {
        let operation = context.get("operation").map(String::as_str).unwrap_or("");
        if operation == "list" && code.unwrap_or(1) == 0 {
            let request_id = context
                .get("request_id")
                .and_then(|value| value.parse().ok())
                .unwrap_or(0);
            if request_id < self.applied_list_request {
                return;
            }
            match serde_json::from_slice::<Vec<AgentRecord>>(&stdout) {
                Ok(agents) => {
                    self.applied_list_request = request_id;
                    self.agents = agents;
                    self.clamp_selection();
                    if self.notice.starts_with("Refreshing") {
                        self.notice = "Metadata refreshed".into();
                    }
                }
                Err(error) => self.notice = format!("Could not read agent state: {error}"),
            }
        } else if operation != "list" {
            if code.unwrap_or(1) == 0 {
                self.notice = format!("{operation} complete");
                self.refresh(false);
            } else {
                let message = String::from_utf8_lossy(&stderr);
                self.notice = truncate(&format!("{operation} failed: {}", message.trim()), 120);
            }
        }
    }
}

impl ZellijPlugin for AgentDeck {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        self.helper = configuration
            .get("helper")
            .cloned()
            .unwrap_or_else(|| "zellij-agent-deck".into());
        self.current_session = get_session_environment_variables()
            .remove("ZELLIJ_SESSION_NAME")
            .unwrap_or_default();
        request_permission(&[
            PermissionType::ReadApplicationState,
            PermissionType::ChangeApplicationState,
            PermissionType::RunCommands,
        ]);
        subscribe(&[
            EventType::Key,
            EventType::Mouse,
            EventType::Visible,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
        ]);
        set_selectable(true);
        set_timeout(3.0);
        self.refresh(false);
        hide_self();
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            Event::Key(key) => {
                self.handle_key(key);
                return true;
            }
            Event::Mouse(Mouse::ScrollDown(_)) => {
                self.move_selection(1);
                return true;
            }
            Event::Mouse(Mouse::ScrollUp(_)) => {
                self.move_selection(-1);
                return true;
            }
            Event::Mouse(Mouse::LeftClick(line, _)) if line >= 3 => {
                self.selected = (line as usize).saturating_sub(3);
                self.clamp_selection();
                return true;
            }
            Event::Visible(visible) => {
                self.visible = visible;
                if visible {
                    self.refresh(false);
                }
            }
            Event::Timer(_) => {
                self.refresh_ticks = self.refresh_ticks.wrapping_add(1);
                self.refresh(false);
                set_timeout(3.0);
            }
            Event::RunCommandResult(code, stdout, stderr, context) => {
                self.handle_result(code, stdout, stderr, context);
                return true;
            }
            Event::PermissionRequestResult(PermissionStatus::Denied) => {
                self.notice = "Agent Deck permissions were denied".into();
            }
            _ => return false,
        }
        self.visible
    }

    fn pipe(&mut self, pipe_message: PipeMessage) -> bool {
        if pipe_message.name == "agent-event" {
            if let Some(payload) = pipe_message.payload {
                if let Ok(agent) = serde_json::from_str::<AgentRecord>(&payload) {
                    self.apply_agent_signal(agent);
                    return true;
                }
            }
        } else if pipe_message.name == "toggle" {
            show_self(true);
        }
        self.visible
    }

    fn render(&mut self, rows: usize, cols: usize) {
        let width = cols.saturating_sub(2);
        let unread = self.agents.iter().filter(|agent| agent.unread).count();
        let waiting = self
            .agents
            .iter()
            .filter(|agent| agent.status == "needs_input")
            .count();
        let header = truncate(
            &format!(
                " Agent Deck  {} agents · {} unread · {} waiting",
                self.agents.len(),
                unread,
                waiting
            ),
            width,
        );
        print_text_with_coordinates(Text::new(header).color_all(3), 1, 0, Some(width), None);

        let filters = FILTERS
            .iter()
            .enumerate()
            .map(|(index, name)| {
                if self.filter == index {
                    format!("[{}:{}]", index + 1, name)
                } else {
                    format!(" {}:{} ", index + 1, name)
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        print_text_with_coordinates(
            Text::new(truncate(&filters, width)).dim_all(),
            1,
            1,
            Some(width),
            None,
        );

        let matching = self.matching_indices();
        let list_height = rows.saturating_sub(8);
        let scroll = self.selected.saturating_sub(list_height.saturating_sub(1));
        for (screen_index, agent_index) in
            matching.iter().skip(scroll).take(list_height).enumerate()
        {
            let agent = &self.agents[*agent_index];
            let cursor = if scroll + screen_index == self.selected {
                "›"
            } else {
                " "
            };
            let unread_mark = if agent.unread { "●" } else { " " };
            let kind = if agent.kind == "subagent" { "↳" } else { " " };
            let state = status_symbol(&agent.status);
            let label = format!(
                "{cursor}{unread_mark}{state}{kind} {}: {}",
                agent.project, agent.title
            );
            let text = Text::new(truncate(&label, width));
            let text = if scroll + screen_index == self.selected {
                text.color_all(3)
            } else {
                text
            };
            print_text_with_coordinates(text, 1, 3 + screen_index, Some(width), None);
        }

        if let Some(agent) = self.selected_agent() {
            let detail_y = rows.saturating_sub(4);
            let dirty = if agent.dirty { "*" } else { "" };
            let ports = if agent.ports.is_empty() {
                String::new()
            } else {
                format!(
                    " ports:{}",
                    agent
                        .ports
                        .iter()
                        .map(u16::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                )
            };
            let pr = if agent.pr.is_empty() {
                String::new()
            } else {
                format!(" {}", agent.pr)
            };
            let detail = format!(
                " {} · {}{}{}{} · {}",
                agent.zellij_session, agent.branch, dirty, pr, ports, agent.status
            );
            print_text_with_coordinates(
                Text::new(truncate(&detail, width)).dim_all(),
                1,
                detail_y,
                Some(width),
                None,
            );
            if !agent.message.is_empty() {
                print_text_with_coordinates(
                    Text::new(truncate(&format!(" {}", agent.message), width)),
                    1,
                    detail_y + 1,
                    Some(width),
                    None,
                );
            }
        }

        let prompt_y = rows.saturating_sub(2);
        let prompt = if matches!(
            self.mode,
            InputMode::Browse | InputMode::ConfirmReply | InputMode::ConfirmPark
        ) {
            self.notice.clone()
        } else {
            format!("{}: {}_", self.notice, self.input)
        };
        print_text_with_coordinates(
            Text::new(truncate(&prompt, width)),
            1,
            prompt_y,
            Some(width),
            None,
        );
        let keys = " Enter jump · r reply · t title · w worktree · p park · R resume · m read · d dismiss · g refresh · / search · q close ";
        print_text_with_coordinates(
            Text::new(truncate(keys, width)).dim_all(),
            1,
            rows.saturating_sub(1),
            Some(width),
            None,
        );
    }
}

fn status_symbol(status: &str) -> &'static str {
    match status {
        "working" => "◐",
        "needs_input" => "!",
        "done" => "✓",
        "parked" => "Ⅱ",
        "ended" => "×",
        _ => "○",
    }
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    if limit <= 1 {
        return "…".chars().take(limit).collect();
    }
    let mut result = value.chars().take(limit - 1).collect::<String>();
    result.push('…');
    result
}

register_plugin!(AgentDeck);

#[cfg(test)]
mod tests {
    use super::*;

    // The Zellij SDK imports this host function even when a unit test does not
    // exercise a host command. Native tests provide a no-op implementation so
    // the test binary can link outside the WASM host.
    #[no_mangle]
    extern "C" fn host_run_plugin_command() {}

    #[test]
    fn truncates_on_character_boundaries() {
        assert_eq!(truncate("example-project", 6), "examp…");
        assert_eq!(truncate("سلام", 3), "سل…");
    }

    #[test]
    fn status_symbols_are_distinct() {
        assert_ne!(status_symbol("working"), status_symbol("needs_input"));
        assert_ne!(status_symbol("done"), status_symbol("parked"));
    }

    #[test]
    fn handled_key_requests_redraw_when_visibility_event_was_missed() {
        let mut deck = AgentDeck::default();

        assert!(deck.update(Event::Key(KeyWithModifier::new(BareKey::Down))));
    }

    #[test]
    fn completed_dismiss_requests_redraw_when_visibility_event_was_missed() {
        let mut deck = AgentDeck::default();
        let context = AgentDeck::context("dismiss");

        assert!(deck.update(Event::RunCommandResult(
            Some(0),
            Vec::new(),
            Vec::new(),
            context,
        )));
    }

    #[test]
    fn new_agent_event_requests_redraw_when_visibility_event_was_missed() {
        let mut deck = AgentDeck::default();
        let payload = serde_json::to_string(&AgentRecord {
            key: "codex:new".into(),
            project: "example".into(),
            title: "new task".into(),
            ..Default::default()
        })
        .unwrap();
        let message = PipeMessage::new(
            PipeSource::Cli("test".into()),
            "agent-event",
            &Some(payload),
            &None,
            false,
        );

        assert!(deck.pipe(message));
        assert_eq!(deck.agents.len(), 1);
    }

    #[test]
    fn jump_within_current_session_focuses_the_terminal_pane() {
        let agent = AgentRecord {
            key: "codex:example".into(),
            zellij_session: "work".into(),
            pane_id: Some(7),
            ..Default::default()
        };

        assert_eq!(
            jump_plan("work", &agent),
            Ok(vec![
                JumpAction::MarkRead {
                    key: "codex:example".into(),
                },
                JumpAction::HideDeck,
                JumpAction::FocusTerminalPane { pane_id: 7 },
            ])
        );
    }

    #[test]
    fn stale_list_result_cannot_restore_a_dismissed_agent() {
        let mut deck = AgentDeck::default();
        let mut latest = AgentDeck::context("list");
        latest.insert("request_id".into(), "2".into());
        let mut stale = AgentDeck::context("list");
        stale.insert("request_id".into(), "1".into());
        let dismissed = serde_json::to_vec(&vec![AgentRecord {
            key: "codex:dismissed".into(),
            ..Default::default()
        }])
        .unwrap();

        deck.handle_result(Some(0), b"[]".to_vec(), Vec::new(), latest);
        deck.handle_result(Some(0), dismissed, Vec::new(), stale);

        assert!(deck.agents.is_empty());
    }

    #[test]
    fn jump_hides_deck_before_switching_to_terminal_pane() {
        let agent = AgentRecord {
            key: "codex:example".into(),
            zellij_session: "work".into(),
            pane_id: Some(7),
            ..Default::default()
        };

        assert_eq!(
            jump_plan("deck", &agent),
            Ok(vec![
                JumpAction::MarkRead {
                    key: "codex:example".into(),
                },
                JumpAction::HideDeck,
                JumpAction::SwitchSession {
                    session: "work".into(),
                    pane_id: 7,
                },
            ])
        );
    }
}
