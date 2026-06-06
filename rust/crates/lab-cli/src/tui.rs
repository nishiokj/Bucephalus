use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use lab_analysis;
use ratatui::{
    prelude::*,
    widgets::{Block, BorderType, Borders, Cell, Gauge, Paragraph, Row, Table, TableState, Wrap},
};
use serde_json::Value;
use std::io::{self, Stdout};
use std::time::Duration;

use crate::view_layout::{self, event_content_preview};
use crate::view_spec::Category;

const APP_BG: Color = Color::Rgb(14, 18, 22);
const PANEL_BG: Color = Color::Rgb(24, 29, 34);
const PANEL_ALT_BG: Color = Color::Rgb(30, 36, 42);
const BORDER: Color = Color::Rgb(63, 74, 82);
const ACCENT: Color = Color::Rgb(102, 212, 196);
const ACCENT_SOFT: Color = Color::Rgb(41, 66, 70);
const TEXT: Color = Color::Rgb(236, 232, 224);
const MUTED: Color = Color::Rgb(144, 153, 160);
const SUCCESS: Color = Color::Rgb(122, 229, 130);
const WARNING: Color = Color::Rgb(255, 194, 82);
const DANGER: Color = Color::Rgb(255, 120, 112);

pub enum Action {
    Quit,
    Back,
    Select,
    Refresh,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    Tick,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyHint {
    pub key: &'static str,
    pub label: &'static str,
}

#[derive(Clone, Debug)]
pub struct RunBrowserItem {
    pub run_id: String,
    pub experiment: String,
    pub started_at: String,
    pub status: String,
    pub status_detail: String,
    pub active_trials: usize,
}

#[derive(Clone, Debug)]
pub struct ViewBrowserItem {
    pub name: String,
    pub purpose: String,
    pub category: Option<Category>,
}

pub struct RunBrowserState<'a> {
    pub items: &'a [RunBrowserItem],
    pub refresh_secs: u64,
    pub chrome_title: &'a str,
    pub description: &'a str,
}

pub struct ViewBrowserState<'a> {
    pub run_id: &'a str,
    pub experiment: &'a str,
    pub started_at: &'a str,
    pub status: &'a str,
    pub items: &'a [ViewBrowserItem],
    pub refresh_secs: u64,
    pub chrome_title: &'a str,
}

pub struct ViewState<'a> {
    pub run_id: &'a str,
    pub status: &'a str,
    pub started_at: &'a str,
    pub view_name: &'a str,
    pub interval_secs: u64,
    pub table: &'a lab_analysis::QueryTable,
    pub display_mode: DisplayMode,
    pub progress: Option<(usize, usize)>,
    pub legend: &'a [(String, String)],
    pub split_labels: Option<(&'a str, &'a str)>,
    pub hints: &'a [KeyHint],
}

pub struct DetailState<'a> {
    pub run_id: &'a str,
    pub view_name: &'a str,
    pub row_label: &'a str,
    pub fields: &'a [(String, String)],
    pub payload: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayMode {
    Overview,
    Table,
    Scoreboard,
    Timeline,
    Comparison,
}

pub enum Screen<'a> {
    RunBrowser(RunBrowserState<'a>),
    ViewBrowser(ViewBrowserState<'a>),
    LiveView(ViewState<'a>),
    Detail(DetailState<'a>),
}

pub struct Term {
    terminal: ratatui::Terminal<CrosstermBackend<Stdout>>,
    table_state: TableState,
}

impl Term {
    pub fn new() -> anyhow::Result<Self> {
        enable_raw_mode()?;
        io::stdout().execute(EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(io::stdout());
        let terminal = ratatui::Terminal::new(backend)?;
        Ok(Self {
            terminal,
            table_state: TableState::default(),
        })
    }

    pub fn draw(&mut self, screen: &Screen) -> anyhow::Result<()> {
        let table_state = &mut self.table_state;
        self.terminal.draw(|f| render(f, screen, table_state))?;
        Ok(())
    }

    pub fn poll(&self, timeout: Duration) -> anyhow::Result<Action> {
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    return Ok(match key.code {
                        KeyCode::Char('q') => Action::Quit,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Action::Quit
                        }
                        KeyCode::Char('r') => Action::Refresh,
                        KeyCode::Esc | KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h') => {
                            Action::Back
                        }
                        KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => Action::Select,
                        KeyCode::Up | KeyCode::Char('k') => Action::ScrollUp,
                        KeyCode::Down | KeyCode::Char('j') => Action::ScrollDown,
                        KeyCode::PageUp => Action::PageUp,
                        KeyCode::PageDown => Action::PageDown,
                        _ => Action::Tick,
                    });
                }
            }
        }
        Ok(Action::Tick)
    }

    pub fn scroll_up(&mut self) {
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some(i.saturating_sub(1)));
    }

    pub fn scroll_down(&mut self, max: usize) {
        if max == 0 {
            self.table_state.select(None);
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state
            .select(Some((i + 1).min(max.saturating_sub(1))));
    }

    pub fn page_up(&mut self) {
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state.select(Some(i.saturating_sub(12)));
    }

    pub fn page_down(&mut self, max: usize) {
        if max == 0 {
            self.table_state.select(None);
            return;
        }
        let i = self.table_state.selected().unwrap_or(0);
        self.table_state
            .select(Some((i + 12).min(max.saturating_sub(1))));
    }

    pub fn selected(&self) -> Option<usize> {
        self.table_state.selected()
    }

    pub fn set_selected(&mut self, idx: Option<usize>) {
        self.table_state.select(idx);
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        if let Err(err) = disable_raw_mode() {
            eprintln!("warning: failed to disable terminal raw mode: {}", err);
        }
        if let Err(err) = io::stdout().execute(LeaveAlternateScreen) {
            eprintln!(
                "warning: failed to leave terminal alternate screen: {}",
                err
            );
        }
    }
}

fn render(f: &mut Frame, screen: &Screen, table_state: &mut TableState) {
    match screen {
        Screen::RunBrowser(state) => render_run_browser(f, state, table_state),
        Screen::ViewBrowser(state) => render_view_browser(f, state, table_state),
        Screen::LiveView(state) => render_live_view(f, state, table_state),
        Screen::Detail(state) => render_detail(f, state),
    }
}

fn render_run_browser(f: &mut Frame, state: &RunBrowserState, table_state: &mut TableState) {
    paint_app_background(f);
    let shell = chrome_block(state.chrome_title, "Runs");
    let inner = shell.inner(f.area());
    f.render_widget(shell, f.area());

    let sections = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(12),
        Constraint::Length(2),
    ])
    .split(inner);

    render_browser_header(
        f,
        sections[0],
        "Runs",
        state.description,
        state.refresh_secs,
        state.items.len(),
    );

    if state.items.is_empty() {
        render_empty_panel(
            f,
            sections[1],
            "No active runs right now",
            "This screen keeps polling. Start a run and it will appear here.",
        );
        render_browser_footer(f, sections[2], "q quit", "Runs refresh automatically", None);
        return;
    }

    let selected = ensure_selection(table_state, state.items.len());
    let columns = Layout::horizontal([Constraint::Percentage(63), Constraint::Percentage(37)])
        .split(sections[1]);

    let header = Row::new(["run", "experiment", "started", "state", "active"])
        .style(table_header_style())
        .height(1);

    let rows = state.items.iter().enumerate().map(|(idx, item)| {
        let bg = striped_bg(idx);
        Row::new(vec![
            Cell::from(item.run_id.as_str()).style(Style::default().fg(TEXT).bg(bg)),
            Cell::from(item.experiment.as_str()).style(Style::default().fg(TEXT).bg(bg)),
            Cell::from(item.started_at.as_str()).style(Style::default().fg(MUTED).bg(bg)),
            Cell::from(item.status.as_str()).style(status_style(item.status.as_str()).bg(bg)),
            Cell::from(item.active_trials.to_string()).style(
                Style::default()
                    .fg(if item.active_trials > 0 {
                        ACCENT
                    } else {
                        MUTED
                    })
                    .bg(bg),
            ),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(32),
            Constraint::Min(18),
            Constraint::Length(24),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(panel_block("Runs"))
    .row_highlight_style(selected_row_style())
    .column_spacing(1);
    f.render_stateful_widget(table, columns[0], table_state);

    let selected_item = &state.items[selected];
    let details = Text::from(vec![
        key_value_line("Run", selected_item.run_id.as_str()),
        key_value_line("Experiment", selected_item.experiment.as_str()),
        key_value_line("Started", selected_item.started_at.as_str()),
        key_value_line("Status", selected_item.status_detail.as_str()),
        key_value_line("Active trials", &selected_item.active_trials.to_string()),
        Line::default(),
        Line::from(vec![Span::styled(
            "Enter opens the view menu for this run.",
            Style::default().fg(MUTED),
        )]),
    ]);
    let detail_card = Paragraph::new(details)
        .block(panel_block("Selected Run"))
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(PANEL_BG));
    f.render_widget(detail_card, columns[1]);

    render_browser_footer(
        f,
        sections[2],
        "Enter choose run",
        "Esc/q quit",
        Some("↑↓ move"),
    );
}

fn render_view_browser(f: &mut Frame, state: &ViewBrowserState, table_state: &mut TableState) {
    paint_app_background(f);
    let shell = chrome_block(state.chrome_title, state.run_id);
    let inner = shell.inner(f.area());
    f.render_widget(shell, f.area());

    let sections = Layout::vertical([
        Constraint::Length(4),
        Constraint::Min(12),
        Constraint::Length(2),
    ])
    .split(inner);

    render_browser_header(
        f,
        sections[0],
        "Views",
        "Choose the lens: progress, scoreboard, task outcomes, trace, or any other standard surface.",
        state.refresh_secs,
        state.items.len(),
    );

    if state.items.is_empty() {
        render_empty_panel(
            f,
            sections[1],
            "No standard views available",
            "This run exists, but the standardized view surface could not be resolved.",
        );
        render_browser_footer(f, sections[2], "Esc back", "q quit", None);
        return;
    }

    let selected = ensure_selection(table_state, state.items.len());
    let columns = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(sections[1]);

    let header = Row::new(["category", "view", "purpose"])
        .style(table_header_style())
        .height(1);
    let mut rows: Vec<Row> = Vec::new();
    let mut prev_category: Option<Category> = None;
    for (idx, item) in state.items.iter().enumerate() {
        let bg = striped_bg(idx);
        let category_label = if item.category != prev_category {
            prev_category = item.category;
            item.category.map(Category::label).unwrap_or("")
        } else {
            ""
        };
        rows.push(Row::new(vec![
            Cell::from(category_label).style(Style::default().fg(ACCENT).bg(bg)),
            Cell::from(item.name.as_str()).style(
                Style::default()
                    .fg(TEXT)
                    .add_modifier(Modifier::BOLD)
                    .bg(bg),
            ),
            Cell::from(item.purpose.as_str()).style(Style::default().fg(MUTED).bg(bg)),
        ]));
    }
    let table = Table::new(
        rows,
        [
            Constraint::Length(10),
            Constraint::Length(18),
            Constraint::Min(18),
        ],
    )
    .header(header)
    .block(panel_block("Available Views"))
    .row_highlight_style(selected_row_style())
    .column_spacing(2);
    f.render_stateful_widget(table, columns[0], table_state);

    let selected_item = &state.items[selected];
    let details = Text::from(vec![
        key_value_line("Run", state.run_id),
        key_value_line("Experiment", state.experiment),
        key_value_line("Started", state.started_at),
        key_value_line("Status", state.status),
        Line::default(),
        key_value_line("View", selected_item.name.as_str()),
        key_value_line(
            "Category",
            selected_item
                .category
                .map(Category::label)
                .unwrap_or("uncategorized"),
        ),
        Line::default(),
        Line::from(vec![Span::styled(
            selected_item.purpose.as_str(),
            Style::default().fg(TEXT),
        )]),
    ]);
    let detail_card = Paragraph::new(details)
        .block(panel_block("Selection"))
        .wrap(Wrap { trim: true })
        .style(Style::default().bg(PANEL_BG));
    f.render_widget(detail_card, columns[1]);

    render_browser_footer(
        f,
        sections[2],
        "Enter open live view",
        "Esc back",
        Some("↑↓ move"),
    );
}

fn render_live_view(f: &mut Frame, state: &ViewState, table_state: &mut TableState) {
    paint_app_background(f);
    let shell = chrome_block("Bucephalus", state.view_name);
    let inner = shell.inner(f.area());
    f.render_widget(shell, f.area());

    let has_progress = state.progress.is_some();
    let has_legend = !state.legend.is_empty();
    let has_split = state.split_labels.is_some();

    let mut constraints = vec![Constraint::Length(4)];
    if has_progress {
        constraints.push(Constraint::Length(3));
    }
    if has_legend {
        constraints.push(Constraint::Length(3));
    }
    if has_split {
        constraints.push(Constraint::Length(2));
    }
    constraints.push(Constraint::Min(8));
    constraints.push(Constraint::Length(2));
    let sections = Layout::vertical(constraints).split(inner);

    let mut slot = 0;
    render_live_header(f, sections[slot], state);
    slot += 1;

    if has_progress {
        render_gauge(f, sections[slot], state);
        slot += 1;
    }
    if has_legend {
        render_legend(f, sections[slot], state);
        slot += 1;
    }
    if has_split {
        render_split_labels(f, sections[slot], state);
        slot += 1;
    }

    render_data(f, sections[slot], state, table_state);
    slot += 1;
    render_live_footer(f, sections[slot], state);
}

fn paint_app_background(f: &mut Frame) {
    f.render_widget(
        Block::default().style(Style::default().bg(APP_BG)),
        f.area(),
    );
}

fn chrome_block<'a>(title: &'a str, subtitle: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(APP_BG))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            format!(" {} ", subtitle),
            Style::default().fg(MUTED),
        ))
}

fn panel_block<'a>(title: &'a str) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(PANEL_BG))
        .title(Span::styled(
            format!(" {} ", title),
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ))
}

fn render_browser_header(
    f: &mut Frame,
    area: Rect,
    title: &str,
    subtitle: &str,
    refresh_secs: u64,
    count: usize,
) {
    let block = panel_block(title).style(Style::default().bg(PANEL_ALT_BG));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = vec![Line::from(vec![
        Span::styled(
            subtitle,
            Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{} item{}", count, if count == 1 { "" } else { "s" }),
            Style::default().fg(ACCENT),
        ),
    ])];
    if refresh_secs > 0 {
        lines.push(Line::from(vec![Span::styled(
            format!("refresh {}s", refresh_secs),
            Style::default().fg(MUTED),
        )]));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_empty_panel(f: &mut Frame, area: Rect, title: &str, body: &str) {
    let block = panel_block(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Text::from(vec![
        Line::default(),
        Line::from(vec![Span::styled(body, Style::default().fg(MUTED))]),
    ]);
    f.render_widget(
        Paragraph::new(text)
            .style(Style::default().bg(PANEL_BG))
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_browser_footer(
    f: &mut Frame,
    area: Rect,
    primary: &str,
    secondary: &str,
    tertiary: Option<&str>,
) {
    let mut spans = vec![
        Span::styled(primary, Style::default().fg(WARNING)),
        Span::raw("  "),
        Span::styled(secondary, Style::default().fg(MUTED)),
    ];
    if let Some(extra) = tertiary {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(extra, Style::default().fg(MUTED)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(APP_BG)),
        area,
    );
}

fn render_hints_footer(f: &mut Frame, area: Rect, hints: &[KeyHint]) {
    let mut spans = Vec::new();
    for (idx, hint) in hints.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(hint.key, Style::default().fg(WARNING)));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(hint.label, Style::default().fg(MUTED)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(APP_BG)),
        area,
    );
}

fn render_live_header(f: &mut Frame, area: Rect, state: &ViewState) {
    let block = panel_block("Current View").style(Style::default().bg(PANEL_ALT_BG));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Text::from(vec![
        Line::from(vec![
            Span::styled(
                state.run_id,
                Style::default().fg(TEXT).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(state.status, status_style(state.status)),
            Span::raw("  "),
            Span::styled(
                state.view_name,
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ),
        ]),
        {
            let mut spans = vec![
                Span::styled(
                    format!(
                        "{} row{}",
                        state.table.rows.len(),
                        if state.table.rows.len() == 1 { "" } else { "s" }
                    ),
                    Style::default().fg(MUTED),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("started {}", state.started_at),
                    Style::default().fg(MUTED),
                ),
            ];
            if state.interval_secs > 0 {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("refresh {}s", state.interval_secs),
                    Style::default().fg(MUTED),
                ));
            }
            Line::from(spans)
        },
    ]);
    f.render_widget(Paragraph::new(text), inner);
}

fn render_gauge(f: &mut Frame, area: Rect, state: &ViewState) {
    let Some((done, total)) = state.progress else {
        return;
    };
    let ratio = if total > 0 {
        done as f64 / total as f64
    } else {
        0.0
    };
    let block = panel_block("Progress");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let gauge = Gauge::default()
        .ratio(ratio.min(1.0))
        .label(format!(" {}/{} ({:.0}%) ", done, total, ratio * 100.0))
        .gauge_style(Style::default().fg(ACCENT).bg(ACCENT_SOFT));
    f.render_widget(gauge, inner);
}

fn render_legend(f: &mut Frame, area: Rect, state: &ViewState) {
    let block = panel_block("Aliases");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut spans = Vec::new();
    for (idx, (key, value)) in state.legend.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw("  "));
        }
        spans.push(Span::styled(
            key.as_str(),
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(": ", Style::default().fg(MUTED)));
        spans.push(Span::styled(value.as_str(), Style::default().fg(TEXT)));
    }
    f.render_widget(
        Paragraph::new(Line::from(spans))
            .style(Style::default().bg(PANEL_BG))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_split_labels(f: &mut Frame, area: Rect, state: &ViewState) {
    let (left, right) = match state.split_labels {
        Some(pair) => pair,
        None => return,
    };
    let block = panel_block("Panels");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let sep_idx = state
        .table
        .columns
        .iter()
        .position(|c| c == "┃")
        .unwrap_or(state.table.columns.len() / 2);
    let widths = compute_column_widths(state.table, usize::from(inner.width));
    let left_chars: usize = widths[..sep_idx]
        .iter()
        .map(|c| match c {
            Constraint::Length(w) => usize::from(*w) + 1,
            _ => 1,
        })
        .sum();

    let pad = left_chars.saturating_sub(1);
    let left_padded = format!(" {:<width$}", left, width = pad);
    let line = Line::from(vec![
        Span::styled(
            left_padded,
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled("┃ ", Style::default().fg(MUTED)),
        Span::styled(
            right.to_string(),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
    ]);
    f.render_widget(Paragraph::new(line), inner);
}

fn render_data(f: &mut Frame, area: Rect, state: &ViewState, table_state: &mut TableState) {
    match state.display_mode {
        DisplayMode::Overview => render_overview(f, area, state, table_state),
        DisplayMode::Table => render_table(f, area, state, table_state),
        DisplayMode::Scoreboard => render_scoreboard(f, area, state, table_state),
        DisplayMode::Timeline => render_timeline(f, area, state, table_state),
        DisplayMode::Comparison => render_comparison(f, area, state, table_state),
    }
}

fn render_overview(f: &mut Frame, area: Rect, state: &ViewState, table_state: &mut TableState) {
    let block = panel_block("Overview");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if render_empty_data_if_needed(f, inner, state) {
        return;
    }

    let selected = ensure_selection(table_state, state.table.rows.len());
    let row = &state.table.rows[selected];
    let mut lines = Vec::new();

    let mut headline = Vec::new();
    push_overview_metric(&mut headline, state.table, row, "done", SUCCESS);
    push_overview_metric(&mut headline, state.table, row, "active", ACCENT);
    push_overview_metric(&mut headline, state.table, row, "total", TEXT);
    push_overview_metric(&mut headline, state.table, row, "pass%", SUCCESS);
    push_overview_metric(&mut headline, state.table, row, "trusted", SUCCESS);
    push_overview_metric(&mut headline, state.table, row, "errors", DANGER);
    push_overview_metric(&mut headline, state.table, row, "warnings", WARNING);
    if !headline.is_empty() {
        lines.push(Line::from(headline));
        lines.push(Line::default());
    }

    for (idx, column) in state.table.columns.iter().enumerate() {
        let value = row
            .get(idx)
            .map(format_cell_value)
            .unwrap_or_else(|| "·".to_string());
        if value == "·" || value.is_empty() || value == "null" {
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<14}", column),
                Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
            ),
            Span::styled(value, overview_value_style(column)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No rows yet",
            Style::default().fg(MUTED),
        )));
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(TEXT).bg(PANEL_BG))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn push_overview_metric(
    spans: &mut Vec<Span<'static>>,
    table: &lab_analysis::QueryTable,
    row: &[Value],
    column: &str,
    color: Color,
) {
    let Some(idx) = table.columns.iter().position(|c| c == column) else {
        return;
    };
    let value = row
        .get(idx)
        .map(format_cell_value)
        .unwrap_or_else(|| "·".to_string());
    if value == "·" || value.is_empty() || value == "null" {
        return;
    }
    if !spans.is_empty() {
        spans.push(Span::raw("   "));
    }
    spans.push(Span::styled(
        format!("{column} "),
        Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        value,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
}

fn overview_value_style(column: &str) -> Style {
    match column {
        "errors" | "grader_err" | "connector_err" | "untrusted" => Style::default().fg(DANGER),
        "warnings" | "empty" | "unknown" => Style::default().fg(WARNING),
        "trusted" | "pass%" | "done" => Style::default().fg(SUCCESS),
        "active" => Style::default().fg(ACCENT),
        _ => Style::default().fg(TEXT),
    }
}

fn render_table(f: &mut Frame, area: Rect, state: &ViewState, table_state: &mut TableState) {
    let block = panel_block("View");
    let inner = block.inner(area);
    f.render_widget(block, area);

    if state.table.columns.is_empty() {
        f.render_widget(
            Paragraph::new("No rows yet")
                .style(Style::default().fg(MUTED).bg(PANEL_BG))
                .alignment(Alignment::Center),
            inner,
        );
        return;
    }

    let visible: Vec<usize> = (0..state.table.columns.len()).collect();

    let header = Row::new(visible.iter().map(|&col_idx| {
        let column = &state.table.columns[col_idx];
        let style = if column == "┃" {
            Style::default().fg(MUTED).bg(PANEL_BG)
        } else {
            table_header_style().bg(PANEL_BG)
        };
        Cell::from(column.as_str()).style(style)
    }))
    .height(1);

    let rows: Vec<Row> = state
        .table
        .rows
        .iter()
        .enumerate()
        .map(|(idx, row)| {
            let bg = striped_bg(idx);
            Row::new(visible.iter().map(|&col_idx| {
                let value = row.get(col_idx).cloned().unwrap_or(Value::Null);
                let style = cell_style(&state.table.columns, col_idx, &value, bg);
                let rendered = render_for_column(&state.table.columns[col_idx], &value);
                Cell::from(rendered).style(style)
            }))
        })
        .collect();

    let widths = compute_visible_column_widths(state.table, &visible, usize::from(inner.width));

    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(selected_row_style())
        .column_spacing(1);

    ensure_selection(table_state, state.table.rows.len());
    f.render_stateful_widget(table, inner, table_state);
}

fn render_scoreboard(f: &mut Frame, area: Rect, state: &ViewState, table_state: &mut TableState) {
    let block = panel_block("Scoreboard");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if render_empty_data_if_needed(f, inner, state) {
        return;
    }

    let selected = ensure_selection(table_state, state.table.rows.len());
    let row_height = 2usize;
    let visible_rows = (usize::from(inner.height).max(row_height) / row_height).max(1);
    let start = selected.saturating_sub(visible_rows / 2);
    let end = (start + visible_rows).min(state.table.rows.len());
    let mut lines = Vec::new();

    for (row_idx, row) in state.table.rows[start..end].iter().enumerate() {
        let absolute_idx = start + row_idx;
        let bg = if absolute_idx == selected {
            ACCENT_SOFT
        } else {
            striped_bg(absolute_idx)
        };
        let style = Style::default().fg(TEXT).bg(bg);
        let variant = first_present(state.table, row, &["variant", "variant_id"]);
        let task = first_present(state.table, row, &["task", "task_id"]);
        let pass = first_present(state.table, row, &["pass%", "success_rate", "pass_rate"]);
        let metric = first_present(state.table, row, &["metric", "primary_metric_mean"]);
        let lifecycle = first_present(state.table, row, &["lifecycle", "status"]);
        let width = usize::from(inner.width);
        let variant_width = 4usize.max(variant.chars().count()).min(8);
        let tag_budget = 34usize;
        let task_width = width
            .saturating_sub(variant_width + tag_budget + 2)
            .clamp(32, 140);

        let mut header = vec![
            Span::styled(
                pad_or_dash(&view_layout::compact_identifier(&variant), variant_width),
                Style::default()
                    .fg(ACCENT)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                clip(&view_layout::compact_identifier(&task), task_width),
                Style::default()
                    .fg(TEXT)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        push_tag(&mut header, "pass", &pass, bg, status_style(&pass).bg(bg));
        push_tag(
            &mut header,
            "metric",
            &metric,
            bg,
            Style::default().fg(TEXT).bg(bg),
        );
        push_tag(
            &mut header,
            "state",
            &lifecycle,
            bg,
            status_style(&lifecycle).bg(bg),
        );
        lines.push(Line::from(header).style(style));

        let details = compact_nonempty_fields(
            state.table,
            row,
            &[
                "variant",
                "variant_id",
                "task",
                "task_id",
                "pass%",
                "success_rate",
                "pass_rate",
                "metric",
                "primary_metric_mean",
                "lifecycle",
                "status",
            ],
            usize::from(inner.width.saturating_sub(2)),
        );
        lines.push(
            Line::from(vec![
                Span::raw("  "),
                Span::styled(details, Style::default().fg(MUTED).bg(bg)),
            ])
            .style(style),
        );
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(PANEL_BG))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn render_timeline(f: &mut Frame, area: Rect, state: &ViewState, table_state: &mut TableState) {
    let block = panel_block("Event Stream  ·  Enter for detail");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if render_empty_data_if_needed(f, inner, state) {
        return;
    }

    let selected = ensure_selection(table_state, state.table.rows.len());
    let row_height = 2usize;
    let visible_rows = (usize::from(inner.height).max(row_height) / row_height).max(1);
    let start = selected.saturating_sub(visible_rows / 2);
    let end = (start + visible_rows).min(state.table.rows.len());
    let mut lines = Vec::new();

    let payload_idx = state
        .table
        .columns
        .iter()
        .position(|c| c == "payload_json" || c == "payload");

    for (row_idx, row) in state.table.rows[start..end].iter().enumerate() {
        let absolute_idx = start + row_idx;
        let bg = if absolute_idx == selected {
            ACCENT_SOFT
        } else {
            striped_bg(absolute_idx)
        };
        let row_style = Style::default().bg(bg).fg(TEXT);

        let event = row_text(state.table, row, &["event_type", "event"], "event");
        let time = compact_time(&row_text(state.table, row, &["ts", "timestamp"], ""));
        let trial_raw = row_text(state.table, row, &["trial_id", "trial"], "");
        let trial = view_layout::compact_identifier(&trial_raw);
        let tool = row_text(state.table, row, &["tool_name", "tool"], "");
        let status = row_text(
            state.table,
            row,
            &["outcome_status", "status", "status_code"],
            "",
        );
        let model = row_text(state.table, row, &["model_identity", "model"], "");
        let tokens_in = row_text(state.table, row, &["usage_tokens_in", "tokens_in"], "");
        let tokens_out = row_text(state.table, row, &["usage_tokens_out", "tokens_out"], "");

        let mut header = vec![
            Span::styled(pad_or_dash(&time, 8), Style::default().fg(MUTED).bg(bg)),
            Span::raw(" "),
            Span::styled(
                pad_or_dash(&trial, 14),
                Style::default()
                    .fg(TEXT)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                clip(&event, 22),
                Style::default()
                    .fg(ACCENT)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        push_tag(
            &mut header,
            "tool",
            &tool,
            bg,
            Style::default().fg(TEXT).bg(bg),
        );
        push_tag(
            &mut header,
            "status",
            &status,
            bg,
            status_style(&status).bg(bg),
        );
        push_tag(
            &mut header,
            "model",
            &model,
            bg,
            Style::default().fg(TEXT).bg(bg),
        );
        if !tokens_in.is_empty() && !tokens_out.is_empty() {
            header.push(Span::raw("  "));
            header.push(Span::styled("tok ", Style::default().fg(MUTED).bg(bg)));
            header.push(Span::styled(
                format!("{}/{}", tokens_in, tokens_out),
                Style::default().fg(TEXT).bg(bg),
            ));
        } else if !tokens_in.is_empty() {
            header.push(Span::raw("  "));
            header.push(Span::styled("tok in ", Style::default().fg(MUTED).bg(bg)));
            header.push(Span::styled(tokens_in, Style::default().fg(TEXT).bg(bg)));
        } else if !tokens_out.is_empty() {
            header.push(Span::raw("  "));
            header.push(Span::styled("tok out ", Style::default().fg(MUTED).bg(bg)));
            header.push(Span::styled(tokens_out, Style::default().fg(TEXT).bg(bg)));
        }
        lines.push(Line::from(header).style(row_style));

        let preview = payload_idx
            .and_then(|idx| row.get(idx))
            .and_then(|payload| event_content_preview(&event, payload))
            .unwrap_or_default();
        if !preview.is_empty() {
            let clipped = clip(&preview, usize::from(inner.width.saturating_sub(4)));
            lines.push(
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(clipped, Style::default().fg(MUTED).bg(bg)),
                ])
                .style(row_style),
            );
        } else {
            lines.push(Line::from(Span::styled("", Style::default().bg(bg))).style(row_style));
        }
    }

    f.render_widget(
        Paragraph::new(Text::from(lines)).style(Style::default().bg(PANEL_BG)),
        inner,
    );
}

fn push_tag(
    spans: &mut Vec<Span<'static>>,
    label: &str,
    value: &str,
    bg: Color,
    value_style: Style,
) {
    if value.trim().is_empty() || value == "·" {
        return;
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{label} "),
        Style::default().fg(MUTED).bg(bg),
    ));
    spans.push(Span::styled(clip(value, 32), value_style));
}

fn render_comparison(f: &mut Frame, area: Rect, state: &ViewState, table_state: &mut TableState) {
    let block = panel_block("Compare");
    let inner = block.inner(area);
    f.render_widget(block, area);
    if render_empty_data_if_needed(f, inner, state) {
        return;
    }

    let selected = ensure_selection(table_state, state.table.rows.len());
    let row_height = 5usize;
    let visible_rows = (usize::from(inner.height).max(row_height) / row_height).max(1);
    let start = selected.saturating_sub(visible_rows / 2);
    let end = (start + visible_rows).min(state.table.rows.len());
    let mut lines = Vec::new();

    for (row_idx, row) in state.table.rows[start..end].iter().enumerate() {
        let absolute_idx = start + row_idx;
        let bg = if absolute_idx == selected {
            ACCENT_SOFT
        } else {
            striped_bg(absolute_idx)
        };
        let style = Style::default().fg(TEXT).bg(bg);
        let subject = first_present(
            state.table,
            row,
            &[
                "task",
                "task_id",
                "parameter",
                "parameter_name",
                "variant",
                "variant_id",
                "variant_a",
                "A pass%",
            ],
        );
        let subject = compact_id(&subject);
        let headline = first_present(
            state.table,
            row,
            &[
                "change",
                "outcome_change",
                "delta_outcome",
                "outcome",
                "status",
                "lifecycle",
                "effect",
                "B-A",
                "pass%",
            ],
        );
        let mut headline_spans = vec![
            Span::styled(
                pad_or_dash(&subject, 28),
                style.add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(headline.clone(), status_style(&headline).bg(bg)),
        ];
        push_compare_tag(&mut headline_spans, state.table, row, "trials", bg);
        push_compare_tag(&mut headline_spans, state.table, row, "turn", bg);
        lines.push(Line::from(headline_spans).style(style));

        let a = compare_side_line(
            state.table,
            row,
            "A",
            &[
                "a_outcome",
                "a_status",
                "a_result",
                "a_resolved",
                "a_model",
                "a_tokens_in",
                "a_tokens_out",
            ],
            usize::from(inner.width),
        );
        let b = compare_side_line(
            state.table,
            row,
            "B",
            &[
                "b_outcome",
                "b_status",
                "b_result",
                "b_resolved",
                "b_model",
                "b_tokens_in",
                "b_tokens_out",
            ],
            usize::from(inner.width),
        );
        let d = compare_side_line(
            state.table,
            row,
            "Δ",
            &[
                "d_result",
                "d_resolved",
                "d_tokens_in",
                "d_tokens_out",
                "B-A",
                "h",
                "chi2",
                "effect",
            ],
            usize::from(inner.width),
        );
        if a.is_empty() && b.is_empty() && d.is_empty() {
            let summary = compact_nonempty_fields(
                state.table,
                row,
                &[
                    "task", "change", "outcome", "status", "pass%", "metric", "trials", "A pass%",
                    "B pass%", "B-A", "h", "effect", "warnings", "errors",
                ],
                usize::from(inner.width),
            );
            lines.push(
                Line::from(Span::styled(summary, Style::default().fg(TEXT).bg(bg))).style(style),
            );
            lines.push(Line::from(Span::styled("", Style::default().bg(bg))).style(style));
            lines.push(Line::from(Span::styled("", Style::default().bg(bg))).style(style));
        } else {
            lines.push(Line::from(Span::styled(a, Style::default().fg(TEXT).bg(bg))).style(style));
            lines.push(Line::from(Span::styled(b, Style::default().fg(TEXT).bg(bg))).style(style));
            lines.push(Line::from(Span::styled(d, Style::default().fg(MUTED).bg(bg))).style(style));
            lines.push(Line::from(Span::styled("", Style::default().bg(bg))).style(style));
        }
    }

    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(PANEL_BG))
            .wrap(Wrap { trim: true }),
        inner,
    );
}

fn push_compare_tag(
    spans: &mut Vec<Span<'static>>,
    table: &lab_analysis::QueryTable,
    row: &[Value],
    column: &str,
    bg: Color,
) {
    let value = first_present(table, row, &[column]);
    if value.is_empty() {
        return;
    }
    spans.push(Span::raw("  "));
    spans.push(Span::styled(
        format!("{column} "),
        Style::default().fg(MUTED).bg(bg),
    ));
    spans.push(Span::styled(
        clip(&value, 36),
        Style::default().fg(TEXT).bg(bg),
    ));
}

fn compare_side_line(
    table: &lab_analysis::QueryTable,
    row: &[Value],
    label: &str,
    columns: &[&str],
    max_width: usize,
) -> String {
    let mut parts = Vec::new();
    let value_width = (max_width / 5).clamp(18, 48);
    for column in columns {
        let value = first_present(table, row, &[*column]);
        if value.is_empty() {
            continue;
        }
        let name = match *column {
            "a_outcome" | "b_outcome" => "out",
            "a_status" | "b_status" => "st",
            "a_result" | "b_result" | "d_result" => "result",
            "a_resolved" | "b_resolved" | "d_resolved" => "resolved",
            "a_model" | "b_model" => "model",
            "a_tokens_in" | "b_tokens_in" | "d_tokens_in" => "tok_in",
            "a_tokens_out" | "b_tokens_out" | "d_tokens_out" => "tok_out",
            other => other,
        };
        parts.push(format!("{name} {}", clip(&value, value_width)));
    }
    if parts.is_empty() {
        return String::new();
    }
    clip(&format!("{label} {}", parts.join("  ")), max_width)
}

fn compact_nonempty_fields(
    table: &lab_analysis::QueryTable,
    row: &[Value],
    skip_columns: &[&str],
    max_width: usize,
) -> String {
    let mut parts = Vec::new();
    for (idx, column) in table.columns.iter().enumerate() {
        if skip_columns.iter().any(|skip| *skip == column) {
            continue;
        }
        let value = row
            .get(idx)
            .map(format_cell_value)
            .unwrap_or_else(|| "·".to_string());
        if value == "·" || value.is_empty() || value == "null" {
            continue;
        }
        let value_width = (max_width / 4).clamp(18, 56);
        parts.push(format!(
            "{} {}",
            compact_label(column),
            clip(&value, value_width)
        ));
    }
    clip(&parts.join("  "), max_width)
}

fn render_empty_data_if_needed(f: &mut Frame, area: Rect, state: &ViewState) -> bool {
    if state.table.columns.is_empty() || state.table.rows.is_empty() {
        f.render_widget(
            Paragraph::new("No rows yet")
                .style(Style::default().fg(MUTED).bg(PANEL_BG))
                .alignment(Alignment::Center),
            area,
        );
        true
    } else {
        false
    }
}

fn column_index(table: &lab_analysis::QueryTable, names: &[&str]) -> Option<usize> {
    names
        .iter()
        .find_map(|name| table.columns.iter().position(|column| column == name))
}

fn row_text(
    table: &lab_analysis::QueryTable,
    row: &[Value],
    names: &[&str],
    default_text: &str,
) -> String {
    column_index(table, names)
        .and_then(|idx| row.get(idx))
        .map(format_cell_value)
        .filter(|value| value != "·" && !value.is_empty())
        .unwrap_or_else(|| default_text.to_string())
}

fn first_present(table: &lab_analysis::QueryTable, row: &[Value], names: &[&str]) -> String {
    row_text(table, row, names, "")
}

fn compact_id(value: &str) -> String {
    let value = value.trim();
    for prefix in ["trial_", "task_", "run_"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            if rest.len() > 18 {
                return format!("{}…{}", prefix, &rest[rest.len().saturating_sub(12)..]);
            }
        }
    }
    clip(value, 28)
}

fn compact_time(value: &str) -> String {
    if value.len() >= 19 && value.as_bytes().get(10) == Some(&b'T') {
        value[11..19].to_string()
    } else {
        clip(value, 8)
    }
}

fn pad_or_dash(value: &str, width: usize) -> String {
    format!("{:<width$}", clip(value, width), width = width)
}

fn clip(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = value.chars().count();
    if count <= width {
        return value.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    let mut clipped = value
        .chars()
        .take(width.saturating_sub(1))
        .collect::<String>();
    clipped.push('…');
    clipped
}

fn render_for_column(column: &str, value: &Value) -> String {
    let raw = format_cell_value(value);
    if raw.is_empty() || raw == "·" {
        return raw;
    }
    match column {
        "trial_id" | "task_id" | "run_id" | "variant_id" | "variant_a_id" | "variant_b_id"
        | "variant_a_trial_id" | "variant_b_trial_id" | "baseline_id" | "treatment_id"
        | "a_trial_id" | "b_trial_id" | "slot_commit_id" | "call_id" => {
            view_layout::compact_identifier(&raw)
        }
        "ts" | "timestamp" => compact_time(&raw),
        _ => raw,
    }
}

fn compact_label(name: &str) -> String {
    match name {
        "variant_id" | "variant" => "var".to_string(),
        "task_id" | "task" => "task".to_string(),
        "trial_id" | "trial" => "trial".to_string(),
        "event_type" | "event" => "evt".to_string(),
        "outcome_status" | "status_code" | "status" => "st".to_string(),
        "usage_tokens_in" | "tokens_in" | "input_tokens" => "tok_in".to_string(),
        "usage_tokens_out" | "tokens_out" | "output_tokens" => "tok_out".to_string(),
        "primary_metric_value" | "metric_value" => "metric".to_string(),
        "primary_metric_mean" => "mean".to_string(),
        "duration_seconds" => "dur".to_string(),
        "error_message" => "err".to_string(),
        other => other
            .trim_start_matches("variant_")
            .trim_start_matches("delta_")
            .replace("_count", "s")
            .replace('_', "-"),
    }
}

fn render_live_footer(f: &mut Frame, area: Rect, state: &ViewState) {
    if state.hints.is_empty() {
        let line = Line::from(vec![
            Span::styled("Esc", Style::default().fg(WARNING)),
            Span::raw(" "),
            Span::styled("back", Style::default().fg(MUTED)),
            Span::raw("  "),
            Span::styled("q", Style::default().fg(WARNING)),
            Span::raw(" "),
            Span::styled("quit", Style::default().fg(MUTED)),
        ]);
        f.render_widget(Paragraph::new(line), area);
    } else {
        render_hints_footer(f, area, state.hints);
    }
}

fn ensure_selection(table_state: &mut TableState, len: usize) -> usize {
    if len == 0 {
        table_state.select(None);
        return 0;
    }
    let idx = table_state
        .selected()
        .unwrap_or(0)
        .min(len.saturating_sub(1));
    table_state.select(Some(idx));
    idx
}

fn key_value_line(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{}: ", label),
            Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
        ),
        Span::styled(value.to_string(), Style::default().fg(TEXT)),
    ])
}

fn table_header_style() -> Style {
    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
}

fn selected_row_style() -> Style {
    Style::default()
        .bg(ACCENT_SOFT)
        .fg(TEXT)
        .add_modifier(Modifier::BOLD)
}

fn striped_bg(idx: usize) -> Color {
    if idx.is_multiple_of(2) {
        PANEL_BG
    } else {
        PANEL_ALT_BG
    }
}

fn status_style(status: &str) -> Style {
    if status.starts_with("running") {
        Style::default().fg(WARNING).add_modifier(Modifier::BOLD)
    } else if status.starts_with("paused") {
        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else if status == "completed" {
        Style::default().fg(SUCCESS).add_modifier(Modifier::BOLD)
    } else if status == "interrupted" {
        Style::default().fg(WARNING).add_modifier(Modifier::BOLD)
    } else if status.contains("fail") || status.contains("error") || status == "killed" {
        Style::default().fg(DANGER).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(TEXT)
    }
}

fn format_cell_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(number) => {
            if let Some(i) = number.as_i64() {
                i.to_string()
            } else if let Some(u) = number.as_u64() {
                u.to_string()
            } else if let Some(f) = number.as_f64() {
                if f == f.trunc() && f.abs() < 1e15 {
                    format!("{:.0}", f)
                } else {
                    format!("{:.4}", f)
                }
            } else {
                number.to_string()
            }
        }
        Value::Bool(boolean) => boolean.to_string(),
        other => other.to_string(),
    }
}

pub(crate) fn is_metric_column(name: &str) -> bool {
    name.contains("rate")
        || name.contains("score")
        || name.ends_with('%')
        || name == "primary_metric_mean"
        || name == "metric"
        || name == "effect"
}

pub(crate) fn is_outcome_column(name: &str) -> bool {
    name == "outcome" || name.ends_with("_outcome")
}

pub(crate) fn is_status_column(name: &str) -> bool {
    name == "status" || name == "lifecycle"
}

fn cell_style(columns: &[String], col_idx: usize, value: &Value, bg: Color) -> Style {
    let column = columns.get(col_idx).map(String::as_str).unwrap_or("");
    let base = Style::default().bg(bg).fg(TEXT);

    if column == "st" {
        if let Some(symbol) = value.as_str() {
            return match symbol {
                "●" => base.fg(SUCCESS),
                "✗" => base.fg(DANGER),
                _ => base.fg(MUTED),
            };
        }
    }

    if column == "┃" {
        return base.fg(MUTED);
    }

    if is_metric_column(column) {
        if let Some(number) = value.as_f64() {
            return if number >= 0.8 {
                base.fg(SUCCESS)
            } else if number >= 0.5 {
                base.fg(WARNING)
            } else {
                base.fg(DANGER)
            };
        }
    }

    if is_outcome_column(column) {
        if let Some(status) = value.as_str() {
            return match status {
                "success" => base.fg(SUCCESS),
                "failure" | "error" => base.fg(DANGER),
                _ => base,
            };
        }
    }

    if is_status_column(column) {
        if let Some(status) = value.as_str() {
            return status_style(status).bg(bg);
        }
    }

    base
}

fn compute_column_widths(table: &lab_analysis::QueryTable, available: usize) -> Vec<Constraint> {
    let all: Vec<usize> = (0..table.columns.len()).collect();
    compute_visible_column_widths(table, &all, available)
}

fn compute_visible_column_widths(
    table: &lab_analysis::QueryTable,
    visible: &[usize],
    available: usize,
) -> Vec<Constraint> {
    if visible.is_empty() {
        return vec![];
    }

    let desired: Vec<usize> = visible
        .iter()
        .map(|&idx| table.columns[idx].len())
        .collect::<Vec<_>>();
    let mut desired = desired;
    for row in &table.rows {
        for (slot, &col_idx) in visible.iter().enumerate() {
            if let Some(value) = row.get(col_idx) {
                let rendered = render_for_column(&table.columns[col_idx], value);
                desired[slot] = desired[slot].max(rendered.chars().count());
            }
        }
    }

    let separators = visible.len().saturating_sub(1);
    let usable = available.saturating_sub(separators).max(visible.len());
    let desired_total: usize = desired.iter().sum();

    if desired_total <= usable {
        return desired
            .iter()
            .map(|&width| Constraint::Length(width as u16))
            .collect();
    }

    let min_widths: Vec<usize> = visible
        .iter()
        .map(|&idx| match table.columns[idx].as_str() {
            "task" | "task_id" => 12,
            "variant" | "variant_id" => 10,
            "outcome" | "change" | "status" | "lifecycle" => 8,
            _ => table.columns[idx].len().clamp(4, 10),
        })
        .collect();
    let min_total: usize = min_widths.iter().sum();
    if min_total >= usable {
        let base = (usable / visible.len()).max(1);
        let mut widths = vec![base; visible.len()];
        let mut remainder = usable.saturating_sub(base * visible.len());
        let mut slot = 0;
        while remainder > 0 {
            widths[slot] += 1;
            remainder -= 1;
            slot = (slot + 1) % widths.len();
        }
        return widths
            .iter()
            .map(|&width| Constraint::Length(width as u16))
            .collect();
    }

    let mut widths = min_widths;
    let mut remaining = usable - min_total;
    while remaining > 0 {
        let mut grew = false;
        for idx in 0..widths.len() {
            if remaining == 0 {
                break;
            }
            if widths[idx] < desired[idx] {
                widths[idx] += 1;
                remaining -= 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    widths
        .iter()
        .map(|&width| Constraint::Length(width as u16))
        .collect()
}

fn render_detail(f: &mut Frame, state: &DetailState) {
    paint_app_background(f);
    let shell = chrome_block("Bucephalus · Detail", state.view_name);
    let inner = shell.inner(f.area());
    f.render_widget(shell, f.area());

    let sections = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(2),
    ])
    .split(inner);

    let header_block = panel_block("Selected Row").style(Style::default().bg(PANEL_ALT_BG));
    let header_inner = header_block.inner(sections[0]);
    f.render_widget(header_block, sections[0]);
    f.render_widget(
        Paragraph::new(Text::from(vec![
            Line::from(vec![
                Span::styled(
                    state.view_name,
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(state.row_label, Style::default().fg(TEXT)),
            ]),
            Line::from(vec![Span::styled(state.run_id, Style::default().fg(MUTED))]),
        ])),
        header_inner,
    );

    let body_layout = if state.payload.is_some() {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
            .split(sections[1])
    } else {
        Layout::horizontal([Constraint::Percentage(100)]).split(sections[1])
    };

    let fields_block = panel_block("Fields");
    let fields_inner = fields_block.inner(body_layout[0]);
    f.render_widget(fields_block, body_layout[0]);

    let key_width = state
        .fields
        .iter()
        .map(|(k, _)| k.len())
        .max()
        .unwrap_or(0)
        .min(28);
    let lines: Vec<Line> = state
        .fields
        .iter()
        .map(|(key, value)| {
            Line::from(vec![
                Span::styled(
                    format!("{:width$}  ", clip(key, key_width), width = key_width),
                    Style::default().fg(MUTED).add_modifier(Modifier::BOLD),
                ),
                Span::styled(value.clone(), Style::default().fg(TEXT)),
            ])
        })
        .collect();
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(PANEL_BG))
            .wrap(Wrap { trim: false }),
        fields_inner,
    );

    if let Some(payload) = state.payload {
        let payload_block = panel_block("Payload");
        let payload_inner = payload_block.inner(body_layout[1]);
        f.render_widget(payload_block, body_layout[1]);
        f.render_widget(
            Paragraph::new(payload)
                .style(Style::default().fg(TEXT).bg(PANEL_BG))
                .wrap(Wrap { trim: false }),
            payload_inner,
        );
    }

    let footer_line = Line::from(vec![
        Span::styled("Esc", Style::default().fg(WARNING)),
        Span::raw(" "),
        Span::styled("back to view", Style::default().fg(MUTED)),
        Span::raw("  "),
        Span::styled("q", Style::default().fg(WARNING)),
        Span::raw(" "),
        Span::styled("quit", Style::default().fg(MUTED)),
    ]);
    f.render_widget(
        Paragraph::new(footer_line).style(Style::default().bg(APP_BG)),
        sections[2],
    );
}
