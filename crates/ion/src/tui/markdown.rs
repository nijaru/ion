//! Markdown renderer for assistant text, thinking, and history.
//!
//! Pi parity (pi-tui `markdown.js`: marked lexer plus highlight.js
//! treatment), idiomatic Rust: a hand-rolled line parser with no new
//! dependencies, and highlighting for known languages only — pi
//! deliberately skips auto-detection as unreliable, and so do we.
//!
//! Supported subset: ATX and setext headings, fenced code (triple
//! backtick or tilde, streaming-safe unclosed fences, partial-fence
//! trim), indented code, nested/ordered/task lists with hanging
//! indent, blockquotes (`│` border, recursive), GFM tables (box
//! borders, wrapped cells, raw-markdown fallback when narrow), `hr`,
//! paragraphs, and inline strong/em/code/links/images/strikethrough/
//! escapes/autolinks. LaTeX and reference-style links render
//! literally (documented gaps: no LaTeX renderer, rare in agent
//! transcripts).
//!
//! Output is fully wrapped WITH hanging indents at render time, so
//! downstream wrapping (`Transcript`, the fullscreen `Paragraph`) is
//! identity at the same width. On resize only newly-broken rows lose
//! their indent; nothing corrupts. Inline `***` triple-star nesting
//! uses longest-match and can mis-nest (documented simplification).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr as _;

use super::render::{Palette, wrap_affixed, wrap_line};

/// Live drafts stream unbounded text through the band every frame;
/// past this many bytes only the tail renders (with fence resync).
const LIVE_BUDGET: usize = 16_384;

/// Render assistant text to fully-wrapped lines at `width`.
pub fn render(text: &str, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let ctx = Ctx {
        pal: palette,
        base: Style::default(),
        width: width.max(1),
        depth: 0,
    };
    // Pi replaces tabs with 3 spaces before lexing.
    let expanded = text.replace('\t', "   ");
    let lines: Vec<&str> = expanded.lines().collect();
    layout(&ctx, parse_blocks(&ctx, &lines))
}

/// Render thinking text: same blocks, dim italic base composed under
/// every inline style (pi renders thinking as styled markdown too).
pub fn render_thinking(text: &str, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    let dim_italic = Style::new().add_modifier(Modifier::DIM | Modifier::ITALIC);
    let ctx = Ctx {
        pal: palette,
        base: dim_italic,
        width: width.max(1),
        depth: 0,
    };
    let expanded = text.replace('\t', "   ");
    let lines: Vec<&str> = expanded.lines().collect();
    layout(&ctx, parse_blocks(&ctx, &lines))
}

/// Render a streaming draft: tail-capped with fence-state resync so
/// an unclosed fence opened above the cut still renders as code.
pub fn render_live(text: &str, palette: &Palette, width: usize) -> Vec<Line<'static>> {
    if text.len() <= LIVE_BUDGET {
        return render(text, palette, width);
    }
    let mut cut = text.len() - LIVE_BUDGET;
    while cut < text.len() && !text.is_char_boundary(cut) {
        cut += 1;
    }
    let mut start = text[cut..]
        .find('\n')
        .map(|i| cut + i + 1)
        .unwrap_or(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    let start = start.min(text.len());
    let mut tail = String::new();
    if let Some(lang) = fence_at(&text[..start]) {
        tail.push_str("```");
        tail.push_str(&lang);
        tail.push('\n');
    }
    tail.push_str(&text[start..]);
    let mut rows = render(&tail, palette, width);
    rows.insert(
        0,
        Line::from("… (showing recent output)").style(palette.system_note),
    );
    rows
}

/// Fence language open at the end of `head` (`None` when closed):
/// byte scan toggling on fence open/close lines, no styling.
fn fence_at(head: &str) -> Option<String> {
    let mut open: Option<(char, usize, String)> = None;
    for line in head.lines() {
        if let Some((marker, len, lang)) = fence_opener(line) {
            match &open {
                Some((m, l, _)) if *m == marker && is_fence_close(line, marker, *l) => {
                    open = None;
                }
                Some(_) => {}
                None => open = Some((marker, len, lang)),
            }
        } else if let Some((marker, len, _)) = &open
            && is_fence_close(line, *marker, *len)
        {
            open = None;
        }
    }
    open.map(|(_, _, lang)| lang)
}

#[derive(Clone, Copy)]
struct Ctx<'a> {
    pal: &'a Palette,
    base: Style,
    width: usize,
    depth: usize,
}

/// One logical (unwrapped) line plus its wrap affixes: `first` seeds
/// the first row, `cont` every continuation row. Nesting composes by
/// prepending (quote border, list indent); a single top layout pass
/// wraps everything at the outer width.
struct Row {
    first: Vec<(String, Style)>,
    line: Line<'static>,
    cont: Vec<(String, Style)>,
    /// Absolute rows (list items) carry margin-based affixes and
    /// skip outer item prefixes; relative rows (paragraphs, tables,
    /// code) compose under them. Matches pi, where nested lists
    /// render at depth indentation, not item continuation indent.
    absolute: bool,
}

impl Row {
    fn plain(line: Line<'static>) -> Self {
        Self {
            first: vec![],
            line,
            cont: vec![],
            absolute: false,
        }
    }

    fn blank() -> Self {
        Self::plain(Line::from(String::new()))
    }
}

fn layout(ctx: &Ctx, rows: Vec<Row>) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    for row in rows {
        out.extend(wrap_affixed(&row.line, ctx.width, &row.first, &row.cont));
    }
    out
}

impl Ctx<'_> {
    /// Plain text span under the base style (thinking: dim italic).
    fn plain(&self, text: &str) -> Span<'static> {
        Span::styled(text.to_owned(), self.base)
    }

    /// Accent (headings, bullets, links): theme accent under base.
    fn accent(&self) -> Style {
        self.base.patch(self.pal.selector_selected)
    }

    fn dim(&self) -> Style {
        self.base.patch(self.pal.system_note)
    }
}

// ---------------------------------------------------------------------------
// Blocks.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Heading,
    Paragraph,
    Code,
    Table,
    Quote,
    Hr,
    List,
}

fn parse_blocks(ctx: &Ctx, lines: &[&str]) -> Vec<Row> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < lines.len() {
        if lines[pos].trim().is_empty() {
            pos += 1;
            continue;
        }
        let kind = parse_block(ctx, lines, &mut pos, &mut out);
        // Block spacing: collapse blank runs to one spacer; blocks
        // that want air get one when source has none. Paragraphs skip
        // it before lists (pi); tight lists add none themselves.
        let mut had_blank = false;
        while pos < lines.len() && lines[pos].trim().is_empty() {
            had_blank = true;
            pos += 1;
        }
        if pos < lines.len() {
            if had_blank {
                out.push(Row::blank());
            } else {
                match kind {
                    BlockKind::List => {}
                    BlockKind::Paragraph if peek_is_list(lines, pos) => {}
                    _ => out.push(Row::blank()),
                }
            }
        }
    }
    out
}

fn parse_block(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) -> BlockKind {
    let line = lines[*pos];
    if fence_opener(line).is_some() {
        parse_fenced(ctx, lines, pos, out);
        return BlockKind::Code;
    }
    if let Some((level, text)) = atx_heading(line) {
        push_heading(ctx, out, level, text);
        *pos += 1;
        return BlockKind::Heading;
    }
    if is_hr(line) {
        let rule = "─".repeat(ctx.width.clamp(1, 80));
        out.push(Row::plain(Line::from(rule).style(ctx.dim())));
        *pos += 1;
        return BlockKind::Hr;
    }
    if is_table_start(lines, *pos) {
        parse_table(ctx, lines, pos, out);
        return BlockKind::Table;
    }
    if is_quote(line) {
        parse_quote(ctx, lines, pos, out);
        return BlockKind::Quote;
    }
    if parse_bullet(line).is_some() {
        parse_list(ctx, lines, pos, out);
        return BlockKind::List;
    }
    if is_indented_code(line) {
        parse_indented_code(ctx, lines, pos, out);
        return BlockKind::Code;
    }
    parse_paragraph(ctx, lines, pos, out)
}

// --- Headings ---------------------------------------------------------------

/// ATX heading: 1-6 `#`, space (or EOL), optional closing hash run.
fn atx_heading(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    // Up to 3 leading spaces per CommonMark; 4+ is indented code.
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &trimmed[hashes..];
    if !rest.is_empty() && !rest.starts_with([' ', '\t']) {
        return None;
    }
    Some((hashes, strip_closing_hashes(rest.trim())))
}

fn strip_closing_hashes(text: &str) -> &str {
    let mut end = text.len();
    let bytes = text.as_bytes();
    while end > 0 && bytes[end - 1] == b'#' {
        end -= 1;
    }
    if end < text.len() && text[..end].ends_with([' ', '\t']) {
        text[..end].trim_end()
    } else {
        text
    }
}

fn push_heading(ctx: &Ctx, out: &mut Vec<Row>, level: usize, text: &str) {
    let mut style = ctx.accent();
    style = style.patch(Style::new().bold());
    if level == 1 {
        style = style.patch(Style::new().underlined());
    }
    let mut spans = Vec::new();
    if level >= 3 {
        spans.push(Span::styled(format!("{} ", "#".repeat(level)), style));
    }
    for span in inline_spans(ctx, text) {
        spans.push(Span::styled(
            span.content.into_owned(),
            span.style.patch(style),
        ));
    }
    out.push(Row::plain(Line::from(spans)));
}

fn is_setext_h1(line: &str) -> bool {
    let t = line.trim();
    !t.is_empty() && t.chars().all(|c| c == '=')
}

fn is_setext_h2(line: &str) -> bool {
    let t = line.trim();
    // Contiguous dashes only: `- - -` is an hr, `---` after text is h2.
    !t.is_empty() && t.chars().all(|c| c == '-')
}

// --- Fenced code ------------------------------------------------------------

/// Fence opener after ≤3 leading spaces: run of ≥3 backticks/tildes
/// plus the info string. Returns marker char, run length, raw info.
fn fence_opener(line: &str) -> Option<(char, usize, String)> {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return None;
    }
    let marker = trimmed.chars().next()?;
    if marker != '`' && marker != '~' {
        return None;
    }
    let run = trimmed.chars().take_while(|c| *c == marker).count();
    if run < 3 {
        return None;
    }
    Some((marker, run, trimmed[run..].trim().to_owned()))
}

fn is_fence_close(line: &str, marker: char, open_len: usize) -> bool {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() > 3 {
        return false;
    }
    let run = trimmed.chars().take_while(|c| *c == marker).count();
    run >= open_len && trimmed[run..].trim().is_empty()
}

fn parse_fenced(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) {
    let (marker, open_len, info) = fence_opener(lines[*pos]).unwrap_or(('`', 3, String::new()));
    let lang_raw = info.split_whitespace().next().unwrap_or("").to_owned();
    let lang = lang_raw.to_lowercase();
    *pos += 1;
    let mut body: Vec<&str> = Vec::new();
    let mut closed = false;
    while *pos < lines.len() {
        if is_fence_close(lines[*pos], marker, open_len) {
            closed = true;
            *pos += 1;
            break;
        }
        body.push(lines[*pos]);
        *pos += 1;
    }
    if !closed {
        // Streaming partial close (pi #5825): a trailing strict
        // prefix of the opener is a half-typed fence, not content.
        if let Some(last) = body.last() {
            let t = last.trim();
            if !t.is_empty() && t.len() < open_len && t.chars().all(|c| c == marker) {
                body.pop();
            }
        }
    }
    push_code_block(ctx, out, &lang_raw, &lang, &body);
}

fn is_indented_code(line: &str) -> bool {
    !line.trim().is_empty() && line.len() - line.trim_start().len() >= 4
}

fn parse_indented_code(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) {
    let mut body: Vec<&str> = Vec::new();
    while *pos < lines.len() && is_indented_code(lines[*pos]) {
        body.push(lines[*pos].get(4..).unwrap_or(""));
        *pos += 1;
    }
    push_code_block(ctx, out, "", "", &body);
}

fn code_indent() -> Vec<(String, Style)> {
    vec![("  ".to_owned(), Style::default())]
}

fn push_code_block(ctx: &Ctx, out: &mut Vec<Row>, lang_raw: &str, lang: &str, body: &[&str]) {
    let border = ctx.dim();
    out.push(Row::plain(Line::from(Span::styled(
        format!("```{lang_raw}"),
        border,
    ))));
    for code_line in body {
        out.push(Row {
            first: code_indent(),
            line: Line::from(highlight_line(ctx, code_line, lang)),
            cont: code_indent(),
            absolute: false,
        });
    }
    // The closing border always renders, even mid-stream: layout
    // stability beats fence fidelity while tokens arrive.
    out.push(Row::plain(Line::from(Span::styled(
        "```".to_owned(),
        border,
    ))));
}

// --- Tables -----------------------------------------------------------------

fn split_table_row(line: &str) -> Vec<&str> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(str::trim).collect()
}

fn is_delim_cell(cell: &str) -> bool {
    let core = cell.strip_prefix(':').unwrap_or(cell);
    let core = core.strip_suffix(':').unwrap_or(core);
    !core.is_empty() && core.chars().all(|c| c == '-')
}

fn is_table_start(lines: &[&str], pos: usize) -> bool {
    if pos + 1 >= lines.len() || !lines[pos].contains('|') {
        return false;
    }
    let header = split_table_row(lines[pos]);
    let delim = split_table_row(lines[pos + 1]);
    !header.is_empty() && header.len() == delim.len() && delim.iter().all(|c| is_delim_cell(c))
}

#[derive(Clone, Copy)]
enum Align {
    Left,
    Center,
    Right,
}

fn parse_table(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) {
    let start = *pos;
    let header = split_table_row(lines[*pos]);
    let delim = split_table_row(lines[*pos + 1]);
    let ncols = header.len();
    let aligns: Vec<Align> = delim
        .iter()
        .map(|c| {
            let left = c.starts_with(':');
            let right = c.ends_with(':');
            match (left, right) {
                (true, true) => Align::Center,
                (false, true) => Align::Right,
                _ => Align::Left,
            }
        })
        .collect();
    let mut rows: Vec<Vec<&str>> = Vec::new();
    *pos += 2;
    while *pos < lines.len() {
        let line = lines[*pos];
        if line.trim().is_empty() || !line.contains('|') {
            break;
        }
        let mut cells = split_table_row(line);
        cells.resize(ncols, "");
        cells.truncate(ncols);
        rows.push(cells);
        *pos += 1;
    }

    // Styled cells first: inline markup changes widths (links).
    let head_cells: Vec<Vec<Span<'static>>> = header.iter().map(|c| inline_spans(ctx, c)).collect();
    let body_cells: Vec<Vec<Vec<Span<'static>>>> = rows
        .iter()
        .map(|r| r.iter().map(|c| inline_spans(ctx, c)).collect())
        .collect();
    let span_width = |spans: &[Span]| spans.iter().map(|s| s.content.width()).sum::<usize>();
    let mut natural = vec![0usize; ncols];
    for (i, cell) in head_cells.iter().enumerate() {
        natural[i] = natural[i].max(span_width(cell));
    }
    for row in &body_cells {
        for (i, cell) in row.iter().enumerate() {
            natural[i] = natural[i].max(span_width(cell));
        }
    }
    // Narrow fallback (pi): raw markdown lines when columns cannot
    // fit at all.
    let chrome = 3 * ncols + 1;
    if ctx.width < chrome + ncols {
        for line in &lines[start..*pos] {
            out.push(Row::plain(Line::from((*line).to_owned()).style(ctx.base)));
        }
        return;
    }
    let available = ctx.width - chrome;
    let sum_nat: usize = natural.iter().sum();
    let colw: Vec<usize> = if sum_nat <= available {
        natural.clone()
    } else {
        // Longest word per column caps the minimum (cap 10); the
        // remainder distributes proportionally, pi-style.
        let words = |idx: usize| {
            header[idx]
                .split_whitespace()
                .chain(rows.iter().flat_map(|r| r[idx].split_whitespace()))
                .map(|w| w.width())
                .max()
                .unwrap_or(1)
        };
        let mins: Vec<usize> = (0..ncols).map(|i| words(i).clamp(1, 10)).collect();
        let sum_min: usize = mins.iter().sum();
        if sum_min >= available {
            vec![available.max(1) / ncols.max(1); ncols]
        } else if sum_nat == sum_min {
            mins
        } else {
            natural
                .iter()
                .zip(mins.iter())
                .map(|(n, m)| m + (available - sum_min) * (n - m) / (sum_nat - sum_min))
                .collect()
        }
    };

    let border = |s: String| Row::plain(Line::from(s).style(ctx.base));
    let rule = |l: char, m: char, r: char| {
        let mut s = String::from(l);
        for (i, w) in colw.iter().enumerate() {
            if i > 0 {
                s.push(m);
            }
            s.push_str(&"─".repeat(w + 2));
        }
        s.push(r);
        s
    };
    out.push(border(rule('┌', '┬', '┐')));
    for physical in wrap_table_row(&head_cells, &colw, &aligns, true) {
        out.push(Row::plain(physical));
    }
    out.push(border(rule('├', '┼', '┤')));
    for (ri, row) in body_cells.iter().enumerate() {
        for physical in wrap_table_row(row, &colw, &aligns, false) {
            out.push(Row::plain(physical));
        }
        if ri + 1 < body_cells.len() {
            out.push(border(rule('├', '┼', '┤')));
        }
    }
    out.push(border(rule('└', '┴', '┘')));
}

/// Wrap each cell to its column width, pad per alignment, join with
/// `│` borders. Single leading/trailing space padding matches pi.
fn wrap_table_row(
    cells: &[Vec<Span<'static>>],
    colw: &[usize],
    aligns: &[Align],
    header: bool,
) -> Vec<Line<'static>> {
    let wrapped: Vec<Vec<Line<'static>>> = cells
        .iter()
        .zip(colw.iter())
        .map(|(spans, w)| {
            let mut physical = wrap_line(&Line::from(spans.clone()), (*w).max(1));
            if header {
                for line in &mut physical {
                    for span in &mut line.spans {
                        span.style = span.style.patch(Style::new().bold());
                    }
                }
            }
            physical
        })
        .collect();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut out = Vec::new();
    for li in 0..height {
        let mut spans: Vec<Span<'static>> = vec![Span::raw("│ ")];
        for (ci, col) in wrapped.iter().enumerate() {
            if ci > 0 {
                spans.push(Span::raw(" │ "));
            }
            let text: String = col
                .get(li)
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .unwrap_or_default();
            let pad = colw[ci].saturating_sub(text.width());
            let (left, right) = match aligns[ci] {
                Align::Left => (0, pad),
                Align::Right => (pad, 0),
                Align::Center => (pad / 2, pad - pad / 2),
            };
            if left > 0 {
                spans.push(Span::raw(" ".repeat(left)));
            }
            if let Some(line) = col.get(li) {
                spans.extend(line.spans.clone());
            }
            if right > 0 {
                spans.push(Span::raw(" ".repeat(right)));
            }
        }
        spans.push(Span::raw(" │"));
        out.push(Line::from(spans));
    }
    out
}

// --- Quotes -----------------------------------------------------------------

fn is_quote(line: &str) -> bool {
    let trimmed = line.trim_start();
    line.len() - trimmed.len() <= 3 && trimmed.starts_with('>')
}

fn strip_quote(line: &str) -> &str {
    let indent = line.len() - line.trim_start().len();
    let rest = &line[indent + 1..];
    rest.strip_prefix(' ').unwrap_or(rest)
}

fn parse_quote(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) {
    let mut inner: Vec<&str> = Vec::new();
    while *pos < lines.len() {
        let line = lines[*pos];
        if is_quote(line) {
            inner.push(strip_quote(line));
            *pos += 1;
        } else if is_lazy_continuation(lines, *pos, inner.last().copied().unwrap_or("")) {
            inner.push(line.trim_start());
            *pos += 1;
        } else {
            break;
        }
    }
    let inner_ctx = Ctx {
        width: ctx.width.saturating_sub(2).max(1),
        // Pi resets list depth inside quotes: indentation starts
        // at the quote content edge.
        depth: 0,
        ..*ctx
    };
    let mut rows = parse_blocks(&inner_ctx, &inner);
    // Trailing spacers would render a dangling border (pi pops them).
    while rows
        .last()
        .is_some_and(|r| r.first.is_empty() && r.line.spans.is_empty())
    {
        rows.pop();
    }
    let overlay = Style::new().add_modifier(Modifier::DIM | Modifier::ITALIC);
    let border_style = ctx.base.patch(ctx.pal.system_note);
    let border = || vec![("│ ".to_owned(), border_style)];
    for mut row in rows {
        for span in &mut row.line.spans {
            span.style = span.style.patch(overlay);
        }
        let mut first = border();
        first.extend(row.first);
        let mut cont = border();
        cont.extend(row.cont);
        out.push(Row {
            first,
            line: row.line,
            cont,
            absolute: false,
        });
    }
}

/// Lazy continuation: plain text directly under quote text, with no
/// blank gap and no block starter (CommonMark §5.1 laziness).
fn is_lazy_continuation(lines: &[&str], pos: usize, prev_inner: &str) -> bool {
    let line = lines[pos];
    if line.trim().is_empty() || prev_inner.trim().is_empty() {
        return false;
    }
    if fence_opener(line).is_some()
        || atx_heading(line).is_some()
        || is_hr(line)
        || is_quote(line)
        || parse_bullet(line).is_some()
        || is_indented_code(line)
        || is_table_start(lines, pos)
        || is_setext_h1(line)
        || is_setext_h2(line)
    {
        return false;
    }
    true
}

// --- Lists ------------------------------------------------------------------

struct Bullet {
    indent: usize,
    ordered: bool,
    number: u32,
    /// Full marker width: bullet + spaces + task marker.
    marker_width: usize,
    task_done: Option<bool>,
}

fn parse_bullet(line: &str) -> Option<Bullet> {
    let indent = line.len() - line.trim_start().len();
    let rest = line.trim_start();
    let mut chars = rest.chars();
    let first = chars.next()?;
    let (ordered, number, mut used) = if first == '-' || first == '+' || first == '*' {
        (false, 0, 1)
    } else if first.is_ascii_digit() {
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.len() > 9 {
            return None;
        }
        let after = rest[digits.len()..].chars().next()?;
        if after != '.' && after != ')' {
            return None;
        }
        (
            true,
            digits.parse().unwrap_or(1),
            digits.len() + after.len_utf8(),
        )
    } else {
        return None;
    };
    let after = &rest[used..];
    if after.is_empty() {
        // Bare `-` alone: a setext-h2 candidate, not a list item.
        if !ordered {
            return None;
        }
    } else if !after.starts_with([' ', '\t']) {
        return None;
    }
    let spaces = after
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .count();
    used += spaces;
    // Task marker: `[ ]` / `[x]` plus trailing spaces.
    let mut task_done = None;
    let tail = &rest[used..];
    if tail.starts_with('[') && tail.len() >= 3 {
        let mark = tail.chars().nth(1)?;
        if (mark == ' ' || mark == 'x' || mark == 'X') && tail.chars().nth(2) == Some(']') {
            let after_task = &tail[3..];
            if after_task.is_empty() || after_task.starts_with([' ', '\t']) {
                task_done = Some(mark == 'x' || mark == 'X');
                used += 3;
                used += after_task
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .count();
            }
        }
    }
    Some(Bullet {
        indent,
        ordered,
        number,
        marker_width: used,
        task_done,
    })
}

fn peek_is_list(lines: &[&str], pos: usize) -> bool {
    pos < lines.len() && parse_bullet(lines[pos]).is_some()
}

fn parse_list(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) {
    let first = parse_bullet(lines[*pos]).expect("list dispatch guarantees a bullet");
    let base_indent = first.indent;
    let ordered = first.ordered;
    let number = first.number;
    let mut index = 0usize;
    while *pos < lines.len() {
        let bullet = match parse_bullet(lines[*pos]) {
            Some(b) if b.indent == base_indent && b.ordered == ordered => b,
            _ => break,
        };
        let content_indent = bullet.indent + bullet.marker_width;
        let mut body: Vec<&str> = Vec::new();
        // First-line remainder after the marker.
        body.push(
            lines[*pos]
                .trim_start()
                .get(bullet.marker_width..)
                .unwrap_or(""),
        );
        *pos += 1;
        let mut blank_pending = false;
        let mut saw_blank_gap = false;
        while *pos < lines.len() {
            let line = lines[*pos];
            if line.trim().is_empty() {
                blank_pending = true;
                *pos += 1;
                continue;
            }
            let indent = line.len() - line.trim_start().len();
            if indent > bullet.indent {
                if blank_pending {
                    saw_blank_gap = true;
                    body.push("");
                    blank_pending = false;
                }
                // Dedent continuations to the item's content column;
                // nested bullets keep relative indent for recursion.
                let stripped = line.get(content_indent.min(line.len())..).unwrap_or("");
                body.push(if indent >= content_indent {
                    stripped
                } else {
                    line.trim_start()
                });
                *pos += 1;
                continue;
            }
            // Same- or lower-indent bullet ends the item; anything
            // else is lazy continuation or the list's end.
            if parse_bullet(line).is_some() {
                break;
            }
            let lazy_ok = !blank_pending
                && !body.last().is_some_and(|l| l.trim().is_empty())
                && is_plain_text(lines, *pos);
            if indent <= bullet.indent && lazy_ok {
                body.push(line.trim_start());
                *pos += 1;
                continue;
            }
            break;
        }
        while body.last().is_some_and(|l| l.trim().is_empty()) {
            body.pop();
        }
        let loose_item = saw_blank_gap || blank_pending && peek_is_list(lines, *pos);
        // Marker: normalized bullets (pi), honored start numbers.
        let marker = match (ordered, bullet.task_done) {
            (false, None) => "- ".to_owned(),
            (false, Some(done)) => {
                if done {
                    "- [x] ".to_owned()
                } else {
                    "- [ ] ".to_owned()
                }
            }
            (true, None) => format!("{}. ", number + index as u32),
            (true, Some(done)) => {
                if done {
                    format!("{}. [x] ", number + index as u32)
                } else {
                    format!("{}. [ ] ", number + index as u32)
                }
            }
        };
        let item_ctx = Ctx {
            width: ctx
                .width
                .saturating_sub(4 * ctx.depth + marker.width())
                .max(1),
            depth: ctx.depth + 1,
            ..*ctx
        };
        let rows = parse_blocks(&item_ctx, &body);
        let indent_w = 4 * ctx.depth + marker.width();
        let accent = ctx.accent();
        let marker_affix = vec![
            (" ".repeat(4 * ctx.depth), Style::default()),
            (marker, accent),
        ];
        let cont_affix = vec![(" ".repeat(indent_w), Style::default())];
        let mut marked = false;
        for mut row in rows.into_iter() {
            if row.absolute {
                // Nested list: margin-based affixes already, no
                // outer item prefix (pi renders nested lists at
                // depth indentation).
                out.push(row);
                continue;
            }
            // Pi prefixes the first rendered line with the marker
            // even when an absolute block came first.
            let mut first = if marked {
                cont_affix.clone()
            } else {
                marker_affix.clone()
            };
            first.extend(row.first);
            let mut cont = cont_affix.clone();
            cont.extend(row.cont);
            row.first = first;
            row.cont = cont;
            row.absolute = true;
            marked = true;
            out.push(row);
        }
        if loose_item {
            let mut blank = Row::blank();
            blank.absolute = true;
            out.push(blank);
        }
        index += 1;
    }
}

/// Plain-text line: no block starter (lazy list continuation).
fn is_plain_text(lines: &[&str], pos: usize) -> bool {
    let line = lines[pos];
    fence_opener(line).is_none()
        && atx_heading(line).is_none()
        && !is_hr(line)
        && !is_quote(line)
        && parse_bullet(line).is_none()
        && !is_table_start(lines, pos)
        && !is_setext_h1(line)
        && !is_setext_h2(line)
}

// --- Paragraphs -------------------------------------------------------------

fn is_hr(line: &str) -> bool {
    let compact: String = line.chars().filter(|c| !c.is_whitespace()).collect();
    compact.len() >= 3
        && compact
            .chars()
            .all(|c| c == compact.chars().next().unwrap_or('*'))
        && matches!(compact.chars().next(), Some('*' | '-' | '_'))
}

fn parse_paragraph(ctx: &Ctx, lines: &[&str], pos: &mut usize, out: &mut Vec<Row>) -> BlockKind {
    let mut body: Vec<&str> = vec![lines[*pos]];
    *pos += 1;
    while *pos < lines.len() {
        let line = lines[*pos];
        if line.trim().is_empty() {
            break;
        }
        // Setext markers terminate the paragraph (converted below);
        // indented lines are lazy continuation text (they cannot
        // start indented code mid-paragraph).
        if is_setext_h1(line) || is_setext_h2(line) {
            break;
        }
        if fence_opener(line).is_some()
            || atx_heading(line).is_some()
            || is_quote(line)
            || parse_bullet(line).is_some()
            || is_table_start(lines, *pos)
        {
            break;
        }
        if is_hr(line) && !is_setext_h2(line) {
            break;
        }
        body.push(line);
        *pos += 1;
    }
    if *pos < lines.len() {
        if is_setext_h1(lines[*pos]) {
            push_heading(ctx, out, 1, &body.join("\n"));
            *pos += 1;
            return BlockKind::Heading;
        }
        if is_setext_h2(lines[*pos]) {
            push_heading(ctx, out, 2, &body.join("\n"));
            *pos += 1;
            return BlockKind::Heading;
        }
    }
    out.push(Row::plain(Line::from(inline_spans(ctx, &body.join("\n")))));
    BlockKind::Paragraph
}

// ---------------------------------------------------------------------------
// Inline.
// ---------------------------------------------------------------------------

/// Inline markup over one block of text (may contain newlines).
fn inline_spans(ctx: &Ctx, text: &str) -> Vec<Span<'static>> {
    let ch: Vec<char> = text.chars().collect();
    let (spans, _) = parse_inline(ctx, &ch, 0, None);
    spans
}

/// Byte-free prefix test over char slices (avoids an allocation
/// per position in the hot inline/highlight loops).
fn starts_at(ch: &[char], pos: usize, s: &str) -> bool {
    for (i, c) in (pos..).zip(s.chars()) {
        if ch.get(i) != Some(&c) {
            return false;
        }
    }
    true
}

/// Parse until `closer` (or end). Returns spans and the position at
/// the closer (unconsumed) or end of input.
fn parse_inline(
    ctx: &Ctx,
    ch: &[char],
    mut pos: usize,
    closer: Option<&str>,
) -> (Vec<Span<'static>>, usize) {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let flush = |out: &mut Vec<Span<'static>>, plain: &mut String| {
        if !plain.is_empty() {
            out.push(ctx.plain(&std::mem::take(plain)));
        }
    };
    while pos < ch.len() {
        if let Some(c) = closer
            && starts_at(ch, pos, c)
        {
            flush(&mut out, &mut plain);
            return (out, pos);
        }
        let c = ch[pos];
        // Escape: backslash plus ASCII punctuation is literal.
        if c == '\\' && pos + 1 < ch.len() && ch[pos + 1].is_ascii_punctuation() {
            plain.push(ch[pos + 1]);
            pos += 2;
            continue;
        }
        // Code span: matching run length, literal content.
        if c == '`' {
            let run = ch[pos..].iter().take_while(|c| **c == '`').count();
            if let Some(end) = find_code_close(ch, pos + run, run) {
                let mut code: String = ch[pos + run..end].iter().collect();
                // Strip one surrounding space/newline when both
                // sides have it (CommonMark).
                if code.len() >= 2
                    && !code.trim().is_empty()
                    && (code.starts_with(' ') || code.starts_with('\n'))
                    && (code.ends_with(' ') || code.ends_with('\n'))
                {
                    code.remove(0);
                    code.pop();
                }
                flush(&mut out, &mut plain);
                out.push(Span::styled(code, ctx.base.patch(Style::new().cyan())));
                pos = end + run;
                continue;
            }
            plain.push(c);
            pos += 1;
            continue;
        }
        // Strong: longest match first (`***` edge documented).
        if c == '*' && starts_at(ch, pos, "**") {
            match parse_delimited(ctx, ch, pos + 2, "**", Style::new().bold()) {
                Some((spans, next)) => {
                    flush(&mut out, &mut plain);
                    out.extend(spans);
                    pos = next;
                    continue;
                }
                None => {
                    plain.push_str("**");
                    pos += 2;
                    continue;
                }
            }
        }
        if c == '_' && starts_at(ch, pos, "__") && underscore_ok(ch, pos) {
            match parse_delimited(ctx, ch, pos + 2, "__", Style::new().bold()) {
                Some((spans, next)) => {
                    flush(&mut out, &mut plain);
                    out.extend(spans);
                    pos = next;
                    continue;
                }
                None => {
                    plain.push_str("__");
                    pos += 2;
                    continue;
                }
            }
        }
        // Emphasis.
        if c == '*' && !starts_at(ch, pos + 1, "*") && star_opener_ok(ch, pos) {
            match parse_em(ctx, ch, pos + 1) {
                Some((spans, next)) => {
                    flush(&mut out, &mut plain);
                    out.extend(spans);
                    pos = next;
                    continue;
                }
                None => {
                    plain.push(c);
                    pos += 1;
                    continue;
                }
            }
        }
        if c == '_' && !starts_at(ch, pos + 1, "_") && underscore_ok(ch, pos) {
            match parse_em_underscore(ctx, ch, pos + 1) {
                Some((spans, next)) => {
                    flush(&mut out, &mut plain);
                    out.extend(spans);
                    pos = next;
                    continue;
                }
                None => {
                    plain.push(c);
                    pos += 1;
                    continue;
                }
            }
        }
        // Strikethrough.
        if c == '~' && starts_at(ch, pos, "~~") {
            match parse_delimited(ctx, ch, pos + 2, "~~", Style::new().crossed_out()) {
                Some((spans, next)) => {
                    flush(&mut out, &mut plain);
                    out.extend(spans);
                    pos = next;
                    continue;
                }
                None => {
                    plain.push_str("~~");
                    pos += 2;
                    continue;
                }
            }
        }
        // Image before link: `![alt](src)`.
        if c == '!' && pos + 1 < ch.len() && ch[pos + 1] == '[' {
            if let Some((alt, src, next)) = parse_link_target(ch, pos + 1) {
                flush(&mut out, &mut plain);
                let (mut alt_spans, _) = parse_inline(ctx, &alt, 0, None);
                out.append(&mut alt_spans);
                out.push(Span::styled(format!(" ({src})"), ctx.dim()));
                pos = next;
                continue;
            }
            plain.push(c);
            pos += 1;
            continue;
        }
        // Link: `[text](href)`.
        if c == '[' {
            if let Some((text_ch, href, next)) = parse_link_target(ch, pos) {
                flush(&mut out, &mut plain);
                let link_style = Style::new().underlined().patch(ctx.accent());
                let (inner, _) = parse_inline(ctx, &text_ch, 0, None);
                let inner_text: String = inner.iter().map(|s| s.content.as_ref()).collect();
                for span in inner {
                    out.push(Span::styled(
                        span.content.into_owned(),
                        span.style.patch(link_style),
                    ));
                }
                let bare = inner_text == href
                    || href
                        .strip_prefix("mailto:")
                        .is_some_and(|m| m == inner_text);
                if !bare {
                    out.push(Span::styled(format!(" ({href})"), ctx.dim()));
                }
                pos = next;
                continue;
            }
            plain.push(c);
            pos += 1;
            continue;
        }
        // Autolink: `<scheme:...>` or `<mail@host>`.
        if c == '<' {
            if let Some((text, href, next)) = parse_autolink(ch, pos) {
                flush(&mut out, &mut plain);
                let link_style = Style::new().underlined().patch(ctx.accent());
                out.push(Span::styled(text.clone(), ctx.base.patch(link_style)));
                let bare = text == href || href.strip_prefix("mailto:").is_some_and(|m| m == text);
                if !bare {
                    out.push(Span::styled(format!(" ({href})"), ctx.dim()));
                }
                pos = next;
                continue;
            }
            plain.push(c);
            pos += 1;
            continue;
        }
        // Bare URL (GFM autolink).
        if is_bare_url_start(ch, pos) {
            let end = bare_url_end(ch, pos);
            let url: String = ch[pos..end].iter().collect();
            flush(&mut out, &mut plain);
            let link_style = Style::new().underlined().patch(ctx.accent());
            out.push(Span::styled(url, ctx.base.patch(link_style)));
            pos = end;
            continue;
        }
        plain.push(c);
        pos += 1;
    }
    flush(&mut out, &mut plain);
    (out, pos)
}

/// Parse `**content**`-style runs: find the closer, recurse the
/// middle, overlay `style` on every inner span. `None` when open.
fn parse_delimited(
    ctx: &Ctx,
    ch: &[char],
    start: usize,
    closer: &str,
    style: Style,
) -> Option<(Vec<Span<'static>>, usize)> {
    let end = find_closer(ch, start, closer)?;
    let (inner, _) = parse_inline(ctx, &ch[start..end], 0, None);
    let spans = inner
        .into_iter()
        .map(|s| Span::styled(s.content.into_owned(), s.style.patch(style)))
        .collect();
    Some((spans, end + closer.chars().count()))
}

fn find_closer(ch: &[char], mut pos: usize, closer: &str) -> Option<usize> {
    if closer == "*" || closer == "_" {
        let target = if closer == "*" { '*' } else { '_' };
        while pos < ch.len() {
            if ch[pos] == target
                && (pos == 0 || ch[pos - 1] != target)
                && (pos + 1 >= ch.len() || ch[pos + 1] != target)
            {
                return Some(pos);
            }
            pos += 1;
        }
        return None;
    }
    while pos < ch.len() {
        if starts_at(ch, pos, closer) {
            return Some(pos);
        }
        pos += 1;
    }
    None
}

fn parse_em(ctx: &Ctx, ch: &[char], start: usize) -> Option<(Vec<Span<'static>>, usize)> {
    parse_delimited(ctx, ch, start, "*", Style::new().italic())
}

fn parse_em_underscore(
    ctx: &Ctx,
    ch: &[char],
    start: usize,
) -> Option<(Vec<Span<'static>>, usize)> {
    parse_delimited(ctx, ch, start, "_", Style::new().italic())
}

fn find_code_close(ch: &[char], mut pos: usize, run: usize) -> Option<usize> {
    while pos < ch.len() {
        if ch[pos] == '`' {
            let len = ch[pos..].iter().take_while(|c| **c == '`').count();
            if len == run {
                return Some(pos);
            }
            pos += len;
            continue;
        }
        pos += 1;
    }
    None
}

/// `_`/`__` markup only when not alphanumeric on both sides, so
/// `foo_bar` stays literal (CommonMark flanking, simplified).
fn underscore_ok(ch: &[char], pos: usize) -> bool {
    let prev_alnum = pos > 0 && ch[pos - 1].is_alphanumeric();
    let next_alnum = ch.get(pos + 1).is_some_and(|c| c.is_alphanumeric());
    !(prev_alnum && next_alnum)
}

fn star_opener_ok(ch: &[char], pos: usize) -> bool {
    let prev_alnum = pos > 0 && ch[pos - 1].is_alphanumeric();
    let next_alnum = ch.get(pos + 1).is_some_and(|c| c.is_alphanumeric());
    !(prev_alnum && next_alnum)
}

/// Parse `[text](href "title")` at `pos` (the `[`). Returns text
/// chars, display href, and the position past `)`.
fn parse_link_target(ch: &[char], pos: usize) -> Option<(Vec<char>, String, usize)> {
    // Balanced brackets, one nesting level or more.
    let mut depth = 0usize;
    let mut i = pos;
    let mut close = None;
    while i < ch.len() {
        if ch[i] == '[' {
            depth += 1;
        } else if ch[i] == ']' {
            depth -= 1;
            if depth == 0 {
                close = Some(i);
                break;
            }
        } else if ch[i] == '`' {
            // Skip code spans so `]` inside code doesn't unbalance.
            let run = ch[i..].iter().take_while(|c| **c == '`').count();
            if let Some(end) = find_code_close(ch, i + run, run) {
                i = end + run;
                continue;
            }
        }
        i += 1;
    }
    let close = close?;
    if ch.get(close + 1) != Some(&'(') {
        return None;
    }
    let text: Vec<char> = ch[pos + 1..close].to_vec();
    // Destination: `<...>` or bare (one paren level for Wikipedia).
    let mut j = close + 2;
    while j < ch.len() && (ch[j] == ' ' || ch[j] == '\n' || ch[j] == '\t') {
        j += 1;
    }
    let href: String;
    if ch.get(j) == Some(&'<') {
        let end = ch[j + 1..].iter().position(|c| *c == '>')? + j + 1;
        href = ch[j + 1..end].iter().collect();
        j = end + 1;
    } else {
        let mut parens = 0usize;
        let start = j;
        while j < ch.len() {
            match ch[j] {
                '(' => parens += 1,
                ')' if parens > 0 => parens -= 1,
                ')' => break,
                ' ' | '\n' | '\t' if parens == 0 => break,
                _ => {}
            }
            j += 1;
        }
        href = ch[start..j].iter().collect();
        if href.is_empty() {
            return None;
        }
    }
    // Optional title, then `)`.
    while j < ch.len() && (ch[j] == ' ' || ch[j] == '\n' || ch[j] == '\t') {
        j += 1;
    }
    if let Some(&q) = ch.get(j)
        && (q == '"' || q == '\'' || q == '(')
    {
        let want = if q == '(' { ')' } else { q };
        j += 1;
        while j < ch.len() && ch[j] != want {
            j += 1;
        }
        if ch.get(j) != Some(&want) {
            return None;
        }
        j += 1;
        while j < ch.len() && (ch[j] == ' ' || ch[j] == '\n' || ch[j] == '\t') {
            j += 1;
        }
    }
    if ch.get(j) != Some(&')') {
        return None;
    }
    Some((text, href, j + 1))
}

fn parse_autolink(ch: &[char], pos: usize) -> Option<(String, String, usize)> {
    let end = ch[pos..].iter().position(|c| *c == '>')? + pos;
    if ch[pos + 1..end]
        .iter()
        .any(|c| *c == ' ' || *c == '\n' || *c == '<')
    {
        return None;
    }
    let inner: String = ch[pos + 1..end].iter().collect();
    if let Some(colon) = inner.find(':') {
        let scheme = &inner[..colon];
        if !scheme.is_empty()
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        {
            return Some((inner.clone(), inner, end + 1));
        }
        return None;
    }
    if inner.contains('@') && !inner.starts_with('@') && !inner.ends_with('@') {
        return Some((inner.clone(), format!("mailto:{inner}"), end + 1));
    }
    None
}

fn is_bare_url_start(ch: &[char], pos: usize) -> bool {
    let rest: String = ch[pos..].iter().take(8).collect();
    (rest.starts_with("http://") || rest.starts_with("https://"))
        && (pos == 0 || !ch[pos - 1].is_alphanumeric())
}

fn bare_url_end(ch: &[char], pos: usize) -> usize {
    let mut end = pos;
    while end < ch.len() && !ch[end].is_whitespace() && ch[end] != '<' {
        end += 1;
    }
    // Strip trailing punctuation (and unbalanced `)`).
    let mut text: String = ch[pos..end].iter().collect();
    while text.ends_with(['.', ',', ';', ':', '!', '?']) {
        text.pop();
        end -= 1;
    }
    while text.ends_with(')') {
        let opens = text.chars().filter(|c| *c == '(').count();
        let closes = text.chars().filter(|c| *c == ')').count();
        if closes > opens {
            text.pop();
            end -= 1;
        } else {
            break;
        }
    }
    end
}

// ---------------------------------------------------------------------------
// Code highlight: hand-rolled scanner, known languages only.
// ---------------------------------------------------------------------------

/// Highlight one code line. Unknown or missing languages render
/// plain (pi: no auto-detect — it misidentifies prose as keywords).
fn highlight_line(ctx: &Ctx, line: &str, lang: &str) -> Vec<Span<'static>> {
    if lang == "diff" {
        return highlight_diff_line(ctx, line);
    }
    let spec = match lang_spec(lang) {
        Some(spec) => spec,
        None => return vec![ctx.plain(line)],
    };
    let ch: Vec<char> = line.chars().collect();
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut pos = 0;
    let flush = |out: &mut Vec<Span<'static>>, plain: &mut String| {
        if !plain.is_empty() {
            out.push(ctx.plain(&std::mem::take(plain)));
        }
    };
    while pos < ch.len() {
        // Line comment.
        if spec.line_comments.iter().any(|p| starts_at(&ch, pos, p)) {
            flush(&mut out, &mut plain);
            out.push(Span::styled(
                ch[pos..].iter().collect::<String>(),
                ctx.base.patch(Style::new().dark_gray()),
            ));
            break;
        }
        // Block comment.
        if spec.block_comment && starts_at(&ch, pos, "/*") {
            flush(&mut out, &mut plain);
            let mut end = pos + 2;
            let mut depth = 1usize;
            while end < ch.len() && depth > 0 {
                if starts_at(&ch, end, "/*") {
                    depth += 1;
                    end += 2;
                } else if starts_at(&ch, end, "*/") {
                    depth -= 1;
                    end += 2;
                } else {
                    end += 1;
                }
            }
            out.push(Span::styled(
                ch[pos..end.min(ch.len())].iter().collect::<String>(),
                ctx.base.patch(Style::new().dark_gray()),
            ));
            pos = end.min(ch.len());
            continue;
        }
        // String or char.
        if spec.strings && (ch[pos] == '"' || ch[pos] == '\'' || ch[pos] == '`') {
            let quote = ch[pos];
            // Rust lifetime `'a`: quote, word, no closing quote.
            if quote == '\'' && spec.lifetimes && pos + 1 < ch.len() && ch[pos + 1].is_alphabetic()
            {
                let mut end = pos + 1;
                while end < ch.len() && (ch[end].is_alphanumeric() || ch[end] == '_') {
                    end += 1;
                }
                if ch.get(end) != Some(&'\'') {
                    plain.push_str(&ch[pos..end].iter().collect::<String>());
                    pos = end;
                    continue;
                }
            }
            flush(&mut out, &mut plain);
            let mut end = pos + 1;
            let mut closed = false;
            while end < ch.len() {
                if ch[end] == '\\' {
                    end += 2;
                    continue;
                }
                if ch[end] == quote {
                    end += 1;
                    closed = true;
                    break;
                }
                end += 1;
            }
            // Unclosed string runs to end of line, like editors do.
            let _ = closed;
            out.push(Span::styled(
                ch[pos..end.min(ch.len())].iter().collect::<String>(),
                ctx.base.patch(Style::new().green()),
            ));
            pos = end.min(ch.len());
            continue;
        }
        // Number.
        if ch[pos].is_ascii_digit() {
            flush(&mut out, &mut plain);
            let mut end = pos;
            while end < ch.len() && (ch[end].is_alphanumeric() || ch[end] == '_' || ch[end] == '.')
            {
                end += 1;
            }
            out.push(Span::styled(
                ch[pos..end].iter().collect::<String>(),
                ctx.base.patch(Style::new().yellow()),
            ));
            pos = end;
            continue;
        }
        // Word: keyword, function call, type, or plain.
        if ch[pos].is_alphabetic() || ch[pos] == '_' {
            let mut end = pos;
            while end < ch.len() && (ch[end].is_alphanumeric() || ch[end] == '_') {
                end += 1;
            }
            let word: String = ch[pos..end].iter().collect();
            flush(&mut out, &mut plain);
            let style = if spec.keywords.contains(&word.as_str()) {
                Some(Style::new().magenta())
            } else if ch.get(end) == Some(&'(') {
                Some(Style::new().blue())
            } else if word.starts_with(|c: char| c.is_uppercase()) {
                Some(Style::new().cyan())
            } else {
                None
            };
            match style {
                Some(s) => out.push(Span::styled(word, ctx.base.patch(s))),
                None => out.push(ctx.plain(&word)),
            }
            pos = end;
            continue;
        }
        plain.push(ch[pos]);
        pos += 1;
    }
    flush(&mut out, &mut plain);
    if out.is_empty() {
        out.push(ctx.plain(""));
    }
    out
}

fn highlight_diff_line(ctx: &Ctx, line: &str) -> Vec<Span<'static>> {
    let style = if line.starts_with('+') && !line.starts_with("+++") {
        Style::new().green()
    } else if line.starts_with('-') && !line.starts_with("---") {
        Style::new().red()
    } else if line.starts_with("@@") {
        Style::new().cyan()
    } else {
        return vec![ctx.plain(line)];
    };
    vec![Span::styled(line.to_owned(), ctx.base.patch(style))]
}

struct LangSpec {
    keywords: &'static [&'static str],
    line_comments: &'static [&'static str],
    block_comment: bool,
    strings: bool,
    lifetimes: bool,
}

fn lang_spec(lang: &str) -> Option<LangSpec> {
    let canonical = match lang {
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "sh" | "shell" | "zsh" | "fish" => "bash",
        "rs" => "rust",
        "py" | "pyi" | "gyp" => "python",
        "yml" => "yaml",
        "c++" | "cc" | "cxx" | "h" | "hpp" => "cpp",
        "md" | "markdown" | "text" | "txt" | "" => return None,
        other => other,
    };
    match canonical {
        "rust" => Some(LangSpec {
            keywords: &RUST_KW,
            line_comments: &["//"],
            block_comment: true,
            strings: true,
            lifetimes: true,
        }),
        "python" => Some(LangSpec {
            keywords: &PYTHON_KW,
            line_comments: &["#"],
            block_comment: false,
            strings: true,
            lifetimes: false,
        }),
        "javascript" | "typescript" => Some(LangSpec {
            keywords: &JS_KW,
            line_comments: &["//"],
            block_comment: true,
            strings: true,
            lifetimes: false,
        }),
        "bash" => Some(LangSpec {
            keywords: &BASH_KW,
            line_comments: &["#"],
            block_comment: false,
            strings: true,
            lifetimes: false,
        }),
        "json" => Some(LangSpec {
            keywords: &[],
            line_comments: &[],
            block_comment: false,
            strings: true,
            lifetimes: false,
        }),
        "toml" | "yaml" | "ruby" | "perl" | "r" => Some(LangSpec {
            keywords: match canonical {
                "ruby" => &RUBY_KW,
                _ => &[],
            },
            line_comments: &["#"],
            block_comment: false,
            strings: true,
            lifetimes: false,
        }),
        "go" => Some(LangSpec {
            keywords: &GO_KW,
            line_comments: &["//"],
            block_comment: true,
            strings: true,
            lifetimes: false,
        }),
        "c" | "cpp" | "java" => Some(LangSpec {
            keywords: match canonical {
                "java" => &JAVA_KW,
                "c" => &C_KW,
                _ => &CPP_KW,
            },
            line_comments: &["//"],
            block_comment: true,
            strings: true,
            lifetimes: false,
        }),
        "sql" => Some(LangSpec {
            keywords: &SQL_KW,
            line_comments: &["--"],
            block_comment: true,
            strings: true,
            lifetimes: false,
        }),
        "html" | "xml" => Some(LangSpec {
            keywords: &[],
            line_comments: &[],
            block_comment: false,
            strings: true,
            lifetimes: false,
        }),
        "css" => Some(LangSpec {
            keywords: &[],
            line_comments: &[],
            block_comment: true,
            strings: true,
            lifetimes: false,
        }),
        "lua" => Some(LangSpec {
            keywords: &LUA_KW,
            line_comments: &["--"],
            block_comment: false,
            strings: true,
            lifetimes: false,
        }),
        _ => None,
    }
}

const RUST_KW: [&str; 52] = [
    "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn", "for",
    "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref", "return",
    "self", "Self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use", "where",
    "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "union",
];
const PYTHON_KW: [&str; 35] = [
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue",
    "def", "del", "elif", "else", "except", "finally", "for", "from", "global", "if", "import",
    "in", "is", "lambda", "nonlocal", "not", "or", "pass", "raise", "return", "try", "while",
    "with", "yield",
];
const JS_KW: [&str; 48] = [
    "await",
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "enum",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "implements",
    "import",
    "in",
    "instanceof",
    "interface",
    "let",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
    "yield",
    "async",
    "static",
    "get",
    "set",
    "of",
    "from",
    "type",
];
const BASH_KW: [&str; 30] = [
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "in", "function",
    "select", "case", "esac", "break", "continue", "return", "exit", "export", "local", "readonly",
    "declare", "unset", "shift", "trap", "source", "echo", "test", "time",
];
const GO_KW: [&str; 25] = [
    "break",
    "case",
    "chan",
    "const",
    "continue",
    "default",
    "defer",
    "else",
    "fallthrough",
    "for",
    "func",
    "go",
    "goto",
    "if",
    "import",
    "interface",
    "map",
    "package",
    "range",
    "return",
    "select",
    "struct",
    "switch",
    "type",
    "var",
];
const C_KW: [&str; 32] = [
    "auto", "break", "case", "char", "const", "continue", "default", "do", "double", "else",
    "enum", "extern", "float", "for", "goto", "if", "inline", "int", "long", "register",
    "restrict", "return", "short", "signed", "sizeof", "static", "struct", "switch", "typedef",
    "union", "unsigned", "void",
];
const CPP_KW: [&str; 48] = [
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "class",
    "namespace",
    "template",
    "public",
    "private",
    "protected",
    "virtual",
    "friend",
    "new",
    "delete",
    "try",
    "catch",
    "throw",
    "using",
    "typename",
    "bool",
    "nullptr",
];
const JAVA_KW: [&str; 50] = [
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "try",
    "void",
    "while",
    "true",
    "false",
    "null",
];
const RUBY_KW: [&str; 32] = [
    "alias", "and", "begin", "break", "case", "class", "def", "defined", "do", "else", "elsif",
    "end", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo",
    "rescue", "retry", "return", "self", "super", "then", "true", "undef", "yield",
];
const SQL_KW: [&str; 48] = [
    "select", "from", "where", "join", "left", "right", "inner", "outer", "full", "on", "group",
    "by", "order", "having", "limit", "offset", "insert", "into", "values", "update", "set",
    "delete", "create", "table", "index", "view", "drop", "alter", "add", "column", "as", "and",
    "or", "not", "null", "distinct", "count", "sum", "avg", "min", "max", "union", "all", "case",
    "when", "then", "else", "end",
];
const LUA_KW: [&str; 21] = [
    "and", "break", "do", "else", "elif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until",
];

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Modifier};

    use super::*;
    use crate::settings::Theme;

    use super::super::render::palette;

    fn pal() -> Palette {
        palette(Theme::Dark)
    }

    fn contents(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn headings_drop_prefix_for_h1_h2_and_keep_it_for_h3() {
        let lines = render("# Title\n\n## Sub\n\n### Deep\n", &pal(), 40);
        let text = contents(&lines);
        assert_eq!(text, vec!["Title", "", "Sub", "", "### Deep"]);
        assert!(lines[0].spans.iter().all(|s| {
            s.style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        }));
        assert!(
            lines[2]
                .spans
                .iter()
                .all(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn fenced_code_has_borders_indent_and_spacing() {
        let lines = render("```rust\nlet x = 1;\n```\n\nAfter\n", &pal(), 40);
        let text = contents(&lines);
        assert_eq!(text, vec!["```rust", "  let x = 1;", "```", "", "After"]);
    }

    #[test]
    fn unclosed_fence_still_renders_as_code_with_closing_border() {
        let lines = render("```python\ndef f():\n", &pal(), 40);
        let text = contents(&lines);
        assert_eq!(text, vec!["```python", "  def f():", "```"]);
    }

    #[test]
    fn partial_closing_fence_is_trimmed_mid_stream() {
        let lines = render("```rust\nlet x = 1;\n``", &pal(), 40);
        let text = contents(&lines);
        assert_eq!(text, vec!["```rust", "  let x = 1;", "```"]);
    }

    #[test]
    fn rust_highlight_colors_keywords_strings_comments_numbers() {
        let lines = render(
            "```rust\nlet s = \"hi\"; // note\nlet n = 0xFF;\n```\n",
            &pal(),
            40,
        );
        let code: String = lines[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(code, "  let s = \"hi\"; // note");
        let style_of = |frag: &str| {
            lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .find(|s| s.content.contains(frag))
                .map(|s| s.style)
        };
        assert_eq!(style_of("let").unwrap().fg, Some(Color::Magenta));
        assert_eq!(style_of("\"hi\"").unwrap().fg, Some(Color::Green));
        assert_eq!(style_of("// note").unwrap().fg, Some(Color::DarkGray));
        assert_eq!(style_of("0xFF").unwrap().fg, Some(Color::Yellow));
    }

    #[test]
    fn unknown_language_renders_plain() {
        let lines = render("```klingon\nlet x = 1;\n```\n", &pal(), 40);
        assert!(lines[1].spans.iter().all(|s| s.style.fg.is_none()));
    }

    #[test]
    fn diff_fences_color_sign_lines() {
        let lines = render("```diff\n+added\n-removed\n context\n```\n", &pal(), 40);
        let fg = |row: usize| lines[row].spans.last().map(|s| s.style.fg).unwrap();
        assert_eq!(fg(1), Some(Color::Green));
        assert_eq!(fg(2), Some(Color::Red));
        assert_eq!(lines[3].spans.last().unwrap().style.fg, None);
    }

    #[test]
    fn tight_list_has_no_spacers_loose_list_does() {
        let tight = render("- a\n- b\n", &pal(), 40);
        assert_eq!(contents(&tight), vec!["- a", "- b"]);
        let loose = render("- a\n\n- b\n", &pal(), 40);
        assert_eq!(contents(&loose), vec!["- a", "", "- b"]);
    }

    #[test]
    fn nested_ordered_and_task_lists() {
        let lines = render(
            "1. first\n2. second\n   - [x] done\n   - [ ] todo\n",
            &pal(),
            40,
        );
        assert_eq!(
            contents(&lines),
            vec!["1. first", "2. second", "    - [x] done", "    - [ ] todo"]
        );
    }

    #[test]
    fn list_continuation_wraps_with_hanging_indent() {
        let lines = render("- aa bb cc dd ee ff gg hh ii jj kk ll mm\n", &pal(), 20);
        let text = contents(&lines);
        assert!(text.len() > 1);
        assert!(text[0].starts_with("- "));
        for row in text.iter().skip(1) {
            assert!(row.starts_with("  "), "continuation keeps indent: {row:?}");
        }
    }

    #[test]
    fn quote_uses_border_and_wraps_with_border() {
        let lines = render(
            "> hello world, this is a long quoted line here\n",
            &pal(),
            24,
        );
        let text = contents(&lines);
        assert!(text.len() > 1);
        for row in &text {
            assert!(row.starts_with('│'), "border on every row: {row:?}");
        }
    }

    #[test]
    fn table_renders_box_borders() {
        let lines = render("| a | b |\n|---|---|\n| 1 | 2 |\n", &pal(), 40);
        let text = contents(&lines);
        assert_eq!(text.len(), 5);
        assert!(text[0].starts_with('┌'));
        assert!(text[1].starts_with('│'));
        assert!(text[2].starts_with('├'));
        assert!(text[3].starts_with('│'));
        assert!(text[4].starts_with('└'));
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn table_too_narrow_falls_back_to_raw_lines() {
        let lines = render("| a | b |\n|---|---|\n| 1 | 2 |\n", &pal(), 8);
        let text = contents(&lines);
        // Raw source lines survive (wrapped to the narrow width).
        assert!(text.join("\n").contains("| a | b"));
        assert!(!text.iter().any(|l| l.starts_with('\u{250c}')));
    }

    #[test]
    fn inline_markup_styles_spans() {
        let lines = render("**bold** *em* `code` ~~del~~\n", &pal(), 80);
        let style_of = |frag: &str| {
            lines[0]
                .spans
                .iter()
                .find(|s| s.content.contains(frag))
                .map(|s| s.style)
        };
        assert!(
            style_of("bold")
                .unwrap()
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert!(
            style_of("em")
                .unwrap()
                .add_modifier
                .contains(Modifier::ITALIC)
        );
        assert_eq!(style_of("code").unwrap().fg, Some(Color::Cyan));
        assert!(
            style_of("del")
                .unwrap()
                .add_modifier
                .contains(Modifier::CROSSED_OUT)
        );
    }

    #[test]
    fn underscore_inside_words_stays_literal() {
        let lines = render("foo_bar_baz\n", &pal(), 80);
        assert_eq!(contents(&lines), vec!["foo_bar_baz"]);
        assert!(
            lines[0]
                .spans
                .iter()
                .all(|s| !s.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    #[test]
    fn links_show_url_only_when_text_differs() {
        let lines = render(
            "[docs](https://example.com) and https://example.com/x\n",
            &pal(),
            80,
        );
        let text = contents(&lines);
        assert_eq!(text.len(), 1);
        assert!(text[0].contains("docs (https://example.com)"));
        assert!(text[0].contains("https://example.com/x"));
        assert!(!text[0].contains("https://example.com/x (https://example.com/x)"));
    }

    #[test]
    fn setext_and_hr() {
        let lines = render("Title\n===\n\n---\n", &pal(), 40);
        let text = contents(&lines);
        assert_eq!(text[0], "Title");
        assert_eq!(text[1], "");
        assert!(text[2].chars().all(|c| c == '─'));
    }

    #[test]
    fn setext_h2_after_text_is_not_an_hr() {
        let lines = render("Some text\n---\n", &pal(), 40);
        assert_eq!(contents(&lines), vec!["Some text"]);
    }

    #[test]
    fn thinking_composes_dim_italic_under_styles() {
        let lines = render_thinking("**bold** plain\n", &pal(), 80);
        for span in &lines[0].spans {
            assert!(span.style.add_modifier.contains(Modifier::DIM));
            assert!(span.style.add_modifier.contains(Modifier::ITALIC));
        }
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn live_tail_cap_resyncs_open_fences() {
        let mut big = String::from("# Head\n\n```rust\n");
        for i in 0..2000 {
            big.push_str(&format!("let v{i} = {i};\n"));
        }
        let lines = render_live(&big, &pal(), 60);
        let text = contents(&lines);
        assert!(text[0].contains("showing recent output"));
        assert!(text[1].starts_with("```rust"));
        assert_eq!(text[text.len() - 1], "```");
    }
}
