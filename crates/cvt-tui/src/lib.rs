use ratatui::prelude::*;
use ratatui::widgets::*;

pub fn smoke_render(f: &mut Frame, tick: u64) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());
    let tabs = Tabs::new(vec!["Home", "Proxies", "Logs"])
        .block(Block::bordered().title("cvt"))
        .select((tick % 3) as usize)
        .highlight_style(
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);
    let rows: Vec<Row> = (0..3)
        .map(|i| Row::new(vec![Cell::from(format!("n{i}")), Cell::from("12 ms")]))
        .collect();
    let t = Table::new(rows, [Constraint::Length(20), Constraint::Length(8)])
        .header(Row::new(vec!["Node", "Delay"]).style(Style::default().bold()))
        .block(Block::bordered().title("proxies"));
    f.render_widget(t, chunks[1]);
    let g = Gauge::default()
        .ratio(0.5f64)
        .label(Span::raw("0.5"))
        .gauge_style(Style::default().fg(Color::Cyan));
    f.render_widget(g, chunks[2]);
    let _spark = Sparkline::default().data([1u64, 5, 3]);
    let _list = List::new(["a", "b"]).highlight_symbol(">> ");
    let _p = Paragraph::new(Line::from(vec![Span::styled("x", Style::default())]))
        .wrap(Wrap { trim: true });
}
