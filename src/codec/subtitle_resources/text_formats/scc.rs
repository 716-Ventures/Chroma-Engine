//! External Scenarist SCC: one NTSC field carrying the CC1/CC2 services.
//! CEA-608 code assignments were checked against AndroidX Media3's Cea608Decoder:
//! https://github.com/androidx/media/blob/release/libraries/extractor/src/main/java/androidx/media3/extractor/text/cea/Cea608Decoder.java
//! This does not extract broadcast captions from video elementary streams.
use super::Cue;
use anyhow::{Context, Result, bail};

const ROWS: usize = 15;
const COLS: usize = 32;
const MAX_WORDS: usize = 1_000_000;
const MAX_CUES: usize = 100_000;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_TIME: u64 = 30 * 24 * 3600 * 1000;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    color: u8,
    italic: bool,
    underline: bool,
}
impl Style {
    fn attribute(value: u8) -> Self {
        let color = (value >> 1) & 7;
        Self {
            color: if color == 7 { 0 } else { color },
            italic: color == 7,
            underline: value & 1 != 0,
        }
    }
    fn open(self, out: &mut String) {
        const COLORS: [&str; 7] = [
            "ffffff", "00ff00", "0000ff", "00ffff", "ff0000", "ffff00", "ff00ff",
        ];
        if self.color != 0 {
            out.push_str("<c.ttmlc");
            out.push_str(COLORS[usize::from(self.color)]);
            out.push('>');
        }
        if self.italic {
            out.push_str("<i>");
        }
        if self.underline {
            out.push_str("<u>");
        }
    }
    fn close(self, out: &mut String) {
        if self.underline {
            out.push_str("</u>");
        }
        if self.italic {
            out.push_str("</i>");
        }
        if self.color != 0 {
            out.push_str("</c>");
        }
    }
}
#[derive(Clone, Copy)]
struct Cell {
    character: char,
    style: Style,
}
type Screen = [[Option<Cell>; COLS]; ROWS];
const EMPTY: Screen = [[None; COLS]; ROWS];
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pop,
    Paint,
    Roll(usize),
}
struct Channel {
    displayed: Screen,
    pending: Screen,
    row: usize,
    column: usize,
    style: Style,
    mode: Mode,
    captions: bool,
    active: String,
    start: Option<u64>,
    cues: Vec<Cue>,
}
impl Default for Channel {
    fn default() -> Self {
        Self {
            displayed: EMPTY,
            pending: EMPTY,
            row: ROWS - 1,
            column: 0,
            style: Style::default(),
            mode: Mode::Pop,
            captions: true,
            active: String::new(),
            start: None,
            cues: Vec::new(),
        }
    }
}
#[derive(Default)]
struct Budget {
    cues: usize,
    bytes: usize,
}
impl Channel {
    fn memory(&mut self) -> &mut Screen {
        if self.mode == Mode::Pop {
            &mut self.pending
        } else {
            &mut self.displayed
        }
    }
    fn write(&mut self, character: char) -> bool {
        if self.column < COLS {
            let cell = Cell {
                character,
                style: self.style,
            };
            let (row, column) = (self.row, self.column);
            self.memory()[row][column] = Some(cell);
            self.column += 1;
        }
        self.mode != Mode::Pop
    }
    fn backspace(&mut self) -> bool {
        if self.column > 0 {
            self.column -= 1;
            let (row, column) = (self.row, self.column);
            self.memory()[row][column] = None;
        }
        self.mode != Mode::Pop
    }
    fn preamble(&mut self, first: u8, second: u8) -> bool {
        const ROW: [usize; 8] = [11, 1, 3, 12, 14, 5, 7, 9];
        let next = ROW[usize::from(first & 7)] - 1 + usize::from(second & 0x20 != 0);
        let mut changed = false;
        if let Mode::Roll(count) = self.mode
            && next != self.row
        {
            let previous = self.displayed;
            self.displayed = EMPTY;
            for offset in 0..count {
                if let (Some(source), Some(target)) =
                    (self.row.checked_sub(offset), next.checked_sub(offset))
                {
                    self.displayed[target] = previous[source];
                }
            }
            changed = true;
        }
        self.row = next;
        self.column = if second & 0x10 != 0 {
            usize::from((second >> 1) & 7) * 4
        } else {
            0
        };
        self.style = if second & 0x10 != 0 {
            Style {
                underline: second & 1 != 0,
                ..Style::default()
            }
        } else {
            Style::attribute(second)
        };
        changed
    }
    fn control(&mut self, value: u8) -> bool {
        match value {
            0x20 => {
                self.mode = Mode::Pop;
                self.captions = true;
            }
            0x21 if self.captions => return self.backspace(),
            0x24 if self.captions => {
                let (row, column) = (self.row, self.column);
                self.memory()[row][column..].fill(None);
                return self.mode != Mode::Pop;
            }
            0x25..=0x27 => {
                let count = usize::from(value - 0x23);
                if !matches!(self.mode, Mode::Roll(_)) {
                    self.displayed = EMPTY;
                    self.row = ROWS - 1;
                    self.column = 0;
                    self.style = Style::default();
                }
                self.mode = Mode::Roll(count);
                self.captions = true;
                let first = (self.row + 1).saturating_sub(count);
                self.displayed[..first].fill([None; COLS]);
                self.displayed[self.row + 1..].fill([None; COLS]);
                return true;
            }
            0x29 => {
                self.mode = Mode::Paint;
                self.captions = true;
            }
            0x2a | 0x2b => self.captions = false,
            0x2c => {
                self.displayed = EMPTY;
                return true;
            }
            0x2d if self.captions => {
                if let Mode::Roll(count) = self.mode {
                    let first = (self.row + 1).saturating_sub(count);
                    for row in first..self.row {
                        self.displayed[row] = self.displayed[row + 1];
                    }
                    self.displayed[self.row] = [None; COLS];
                    self.column = 0;
                    self.style = Style::default();
                    return true;
                }
            }
            0x2e => self.pending = EMPTY,
            0x2f => {
                std::mem::swap(&mut self.displayed, &mut self.pending);
                self.mode = Mode::Pop;
                self.captions = true;
                self.column = 0;
                self.style = Style::default();
                return true;
            }
            _ => (),
        }
        false
    }
    fn pair(&mut self, first: u8, second: u8) -> bool {
        let code = first & 0xf7;
        if matches!(code, 0x14 | 0x15) && (0x20..=0x2f).contains(&second) {
            return self.control(second);
        }
        if !self.captions {
            return false;
        }
        if (0x10..=0x1f).contains(&first) && (0x40..=0x7f).contains(&second) {
            return self.preamble(first, second);
        }
        if code == 0x11 && (0x20..=0x2f).contains(&second) {
            // Mid-row attributes occupy one cell, unlike a preamble.
            let changed = self.write(' ');
            self.style = Style::attribute(second);
            return changed;
        }
        if code == 0x17 && (0x21..=0x23).contains(&second) {
            self.column = (self.column + usize::from(second - 0x20)).min(COLS);
            return false;
        }
        if code == 0x11 && (0x30..=0x3f).contains(&second) {
            return self.write(SPECIAL[usize::from(second - 0x30)]);
        }
        if matches!(code, 0x12 | 0x13) && (0x20..=0x3f).contains(&second) {
            self.backspace();
            return self.write(
                if code == 0x12 {
                    EXTENDED_ES_FR
                } else {
                    EXTENDED_PT_DE
                }[usize::from(second - 0x20)],
            );
        }
        if first >= 0x20 {
            let mut changed = self.write(basic(first));
            if second >= 0x20 {
                changed |= self.write(basic(second));
            }
            return changed;
        }
        false
    }
    fn render(&self) -> String {
        let nonempty = |row: &[Option<Cell>; COLS]| {
            row.iter()
                .any(|cell| cell.is_some_and(|cell| !cell.character.is_whitespace()))
        };
        let Some(first) = self.displayed.iter().position(nonempty) else {
            return String::new();
        };
        let last = self.displayed.iter().rposition(nonempty).unwrap_or(first);
        let mut text = String::new();
        for (index, row) in self.displayed[first..=last].iter().enumerate() {
            if index > 0 {
                text.push('\n');
            }
            let Some(end) = row
                .iter()
                .rposition(|cell| cell.is_some_and(|cell| !cell.character.is_whitespace()))
            else {
                continue;
            };
            let mut style = Style::default();
            for cell in &row[..=end] {
                let cell = cell.unwrap_or(Cell {
                    character: ' ',
                    style: Style::default(),
                });
                if cell.style != style {
                    style.close(&mut text);
                    cell.style.open(&mut text);
                    style = cell.style;
                }
                match cell.character {
                    '&' => text.push_str("&amp;"),
                    '<' => text.push_str("&lt;"),
                    '>' => text.push_str("&gt;"),
                    character => text.push(character),
                }
            }
            style.close(&mut text);
        }
        text
    }
    fn sync(
        &mut self,
        time: u64,
        duration: Option<u64>,
        budget: &mut Budget,
        final_clear: bool,
    ) -> Result<()> {
        let text = if final_clear {
            String::new()
        } else {
            self.render()
        };
        if text == self.active {
            return Ok(());
        }
        if let Some(start) = self.start {
            let end = duration.map_or(time, |duration| time.min(duration));
            if end > start {
                budget.cues += 1;
                budget.bytes = budget
                    .bytes
                    .checked_add(self.active.len() + 96)
                    .context("SCC output size overflow")?;
                if budget.cues > MAX_CUES || budget.bytes > MAX_OUTPUT {
                    bail!("SCC output limit exceeded");
                }
                self.cues.push(Cue {
                    start,
                    end,
                    text: std::mem::take(&mut self.active),
                });
            }
        }
        self.start = (!text.is_empty()).then_some(time);
        self.active = text;
        Ok(())
    }
}

fn basic(value: u8) -> char {
    match value {
        0x2a => 'á',
        0x5c => 'é',
        0x5e => 'í',
        0x5f => 'ó',
        0x60 => 'ú',
        0x7b => 'ç',
        0x7c => '÷',
        0x7d => 'Ñ',
        0x7e => 'ñ',
        0x7f => '■',
        _ => char::from(value),
    }
}
const SPECIAL: [char; 16] = [
    '®', '°', '½', '¿', '™', '¢', '£', '♪', 'à', ' ', 'è', 'â', 'ê', 'î', 'ô', 'û',
];
const EXTENDED_ES_FR: [char; 32] = [
    'Á', 'É', 'Ó', 'Ú', 'Ü', 'ü', '‘', '¡', '*', '\'', '—', '©', '℠', '•', '“', '”', 'À', 'Â', 'Ç',
    'È', 'Ê', 'Ë', 'ë', 'Î', 'Ï', 'ï', 'Ô', 'Ù', 'ù', 'Û', '«', '»',
];
const EXTENDED_PT_DE: [char; 32] = [
    'Ã', 'ã', 'Í', 'Ì', 'ì', 'Ò', 'ò', 'Õ', 'õ', '{', '}', '\\', '^', '_', '|', '~', 'Ä', 'ä', 'Ö',
    'ö', 'ß', '¥', '¤', '│', 'Å', 'å', 'Ø', 'ø', '┌', '┐', '└', '┘',
];

fn frame(stamp: &str) -> Result<u64> {
    let bytes = stamp.as_bytes();
    if bytes.len() != 11
        || bytes[2] != b':'
        || bytes[5] != b':'
        || !matches!(bytes[8], b':' | b';')
        || [0, 1, 3, 4, 6, 7, 9, 10]
            .iter()
            .any(|index| !bytes[*index].is_ascii_digit())
    {
        bail!("invalid SCC frame timestamp");
    }
    let number = |index| u64::from(bytes[index] - b'0') * 10 + u64::from(bytes[index + 1] - b'0');
    let (hour, minute, second, frame) = (number(0), number(3), number(6), number(9));
    if minute >= 60 || second >= 60 || frame >= 30 {
        bail!("invalid SCC frame timestamp");
    }
    let minutes = hour * 60 + minute;
    let drop = bytes[8] == b';';
    if drop && minute % 10 != 0 && second == 0 && frame < 2 {
        bail!("nonexistent SCC drop-frame label");
    }
    let total = ((hour * 3600 + minute * 60 + second) * 30 + frame)
        .checked_sub(if drop {
            2 * (minutes - minutes / 10)
        } else {
            0
        })
        .context("invalid SCC drop-frame timestamp")?;
    if milliseconds(total)? > MAX_TIME {
        bail!("SCC timestamp exceeds limit");
    }
    Ok(total)
}
fn milliseconds(frame: u64) -> Result<u64> {
    Ok(frame
        .checked_mul(1001)
        .and_then(|n| n.checked_add(15))
        .context("SCC timestamp overflow")?
        / 30)
}

pub(super) fn parse(text: &str, duration: Option<u64>) -> Result<Vec<(String, Vec<Cue>)>> {
    if text.len() > MAX_OUTPUT {
        bail!("SCC input size limit exceeded");
    }
    let mut lines = text.trim_start_matches('\u{feff}').lines();
    if lines.next().map(str::trim) != Some("Scenarist_SCC V1.0") {
        bail!("not Scenarist SCC");
    }
    let mut channels = [Channel::default(), Channel::default()];
    let mut current = 0;
    let mut previous_control = None;
    let mut last_frame = None;
    let mut last_time = 0;
    let mut words = 0;
    let mut budget = Budget::default();
    let mut xds = false;
    for line in lines.filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let initial = frame(fields.next().context("missing SCC timestamp")?)?;
        let mut row_words = 0;
        for (index, word) in fields.enumerate() {
            row_words += 1;
            words += 1;
            if words > MAX_WORDS {
                bail!("SCC word count limit exceeded");
            }
            let frame = initial
                .checked_add(index as u64)
                .context("SCC timestamp overflow")?;
            if last_frame.is_some_and(|previous| frame < previous) {
                bail!("SCC timestamps are not monotonic");
            }
            last_frame = Some(frame);
            let time = milliseconds(frame)?;
            if time > MAX_TIME {
                bail!("SCC timestamp exceeds limit");
            }
            last_time = time;
            if word.len() != 4 || !word.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                bail!("invalid or truncated SCC word");
            }
            let pair = u16::from_str_radix(word, 16)?;
            let (first, second) = ((pair >> 8) as u8, pair as u8);
            if first.count_ones() % 2 != 1 || second.count_ones() % 2 != 1 {
                bail!("invalid SCC odd parity");
            }
            let (first, second) = (first & 0x7f, second & 0x7f);
            if first == 0 && second == 0 {
                continue;
            }
            // Only encoded control-space pairs repeat for transport redundancy.
            // Repeated basic text (e.g. two 6161 words) must remain four letters.
            let repeatable = (0x10..=0x1f).contains(&first);
            if repeatable && previous_control == Some((first, second)) {
                previous_control = None;
                continue;
            }
            previous_control = repeatable.then_some((first, second));
            if (1..=15).contains(&first) {
                xds = true;
                continue;
            }
            if repeatable {
                current = usize::from(first & 8 != 0);
                if matches!(first & 0xf7, 0x14 | 0x15)
                    && matches!(second, 0x20 | 0x25..=0x27 | 0x29..=0x2b | 0x2f)
                {
                    xds = false;
                }
            }
            if xds {
                continue;
            }
            if channels[current].pair(first, second) {
                channels[current].sync(time, duration, &mut budget, false)?;
            }
        }
        if row_words == 0 {
            bail!("SCC line contains no caption words");
        }
    }
    // A source duration is authoritative; standalone files without a final
    // erase get a bounded five-second final display instead of losing it.
    let end = duration.unwrap_or_else(|| last_time.saturating_add(5000).min(MAX_TIME));
    let mut output = Vec::new();
    for (index, mut channel) in channels.into_iter().enumerate() {
        channel.sync(end, duration, &mut budget, true)?;
        if !channel.cues.is_empty() {
            output.push((format!("CC{}", index + 1), channel.cues));
        }
    }
    if output.is_empty() {
        bail!("SCC contains no timed captions");
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn word(first: u8, second: u8) -> String {
        let parity = |value: u8| {
            if value.count_ones().is_multiple_of(2) {
                value | 0x80
            } else {
                value
            }
        };
        format!("{:02x}{:02x}", parity(first), parity(second))
    }
    fn line(stamp: &str, pairs: &[(u8, u8)]) -> String {
        format!(
            "{stamp}\t{}\n",
            pairs
                .iter()
                .map(|(a, b)| word(*a, *b))
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
    fn source(lines: &[String]) -> String {
        format!("Scenarist_SCC V1.0\n\n{}", lines.concat())
    }
    #[test]
    fn pop_on_repeated_controls_text_and_clear_have_seekable_intervals() {
        let input = source(&[
            line(
                "00:00:01:00",
                &[
                    (0x14, 0x20),
                    (0x14, 0x20),
                    (0x14, 0x2e),
                    (0x61, 0x61),
                    (0x61, 0x61),
                    (0x14, 0x2f),
                    (0x14, 0x2f),
                ],
            ),
            line("00:00:03:00", &[(0x14, 0x2c), (0x14, 0x2c)]),
        ]);
        let tracks = parse(&input, Some(5000)).unwrap();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].0, "CC1");
        let cues = &tracks[0].1;
        assert_eq!(cues.len(), 1);
        assert_eq!((cues[0].start, cues[0].end), (1168, 3003));
        assert_eq!(cues[0].text, "aaaa");
        assert!(!(cues[0].start <= 4000 && 4000 < cues[0].end));
    }
    #[test]
    fn two_channels_unicode_extensions_preambles_and_styles_stay_separate() {
        let input = source(&[
            line(
                "00:00:00:00",
                &[
                    (0x14, 0x20),
                    (0x11, 0x4f),
                    (0x41, 0x20),
                    (0x12, 0x20),
                    (0x11, 0x37),
                    (0x14, 0x60),
                    (0x42, 0x20),
                    (0x13, 0x30),
                    (0x14, 0x2f),
                ],
            ),
            line(
                "00:00:01:00",
                &[(0x1c, 0x20), (0x1c, 0x2e), (0x48, 0x49), (0x1c, 0x2f)],
            ),
            line("00:00:03:00", &[(0x14, 0x2c), (0x1c, 0x2c)]),
        ]);
        let tracks = parse(&input, None).unwrap();
        assert_eq!(tracks.len(), 2);
        assert!(tracks[0].1[0].text.contains("<i><u>AÁ♪</u></i>"));
        assert!(tracks[0].1[0].text.contains('\n'));
        assert!(tracks[0].1[0].text.contains("BÄ"));
        assert_eq!(tracks[1].0, "CC2");
        assert_eq!(tracks[1].1[0].text, "HI");
    }
    #[test]
    fn roll_up_two_three_four_rows_erase_non_display_and_backspace() {
        for (command, rows) in [(0x25, 2usize), (0x26, 3), (0x27, 4)] {
            let mut pairs = vec![(0x14, command), (0x14, command)];
            for &text in b"12345" {
                pairs.push((text, 0));
                pairs.push((0x14, 0x2d));
                pairs.push((0x14, 0x2d));
            }
            pairs.extend([
                (b'A', b'X'),
                (0x14, 0x21),
                (0x14, 0x21),
                (b'B', 0),
                (0x14, 0x2e),
            ]);
            let input = source(&[line("00:00:00:00", &pairs)]);
            let tracks = parse(&input, Some(5000)).unwrap();
            let last = tracks[0].1.last().unwrap();
            assert!(last.text.ends_with("AB"));
            assert_eq!(last.text.lines().count(), rows);
            assert!(!last.text.contains('X'));
            assert!(!last.text.contains('1'));
        }
    }
    #[test]
    fn paint_on_respects_columns_tabs_delete_to_end_and_text_service() {
        let input = source(&[line(
            "00:00:00:00",
            &[
                (0x14, 0x29),
                (0x11, 0x42),
                (b'A', b'B'),
                (0x17, 0x21),
                (0x11, 0x2f),
                (b'C', 0),
                (0x14, 0x2a),
                (b'X', b'X'),
                (0x14, 0x29),
                (0x11, 0x50),
                (b'D', 0),
                (0x14, 0x24),
            ],
        )]);
        let tracks = parse(&input, Some(4000)).unwrap();
        let last = tracks[0].1.last().unwrap();
        assert_eq!(last.text, "D");
        assert!(
            tracks[0]
                .1
                .iter()
                .any(|cue| cue.text.contains("<c.ttmlc00ff00>AB"))
        );
        assert!(!tracks[0].1.iter().any(|cue| cue.text.contains('X')));
        let mut channel = Channel {
            mode: Mode::Paint,
            ..Channel::default()
        };
        for _ in 0..1000 {
            channel.write('a');
        }
        assert_eq!(channel.render().chars().count(), 32);
    }
    #[test]
    fn ntsc_drop_and_non_drop_are_rational_and_missing_labels_fail() {
        assert_eq!(milliseconds(frame("00:01:00:00").unwrap()).unwrap(), 60060);
        assert_eq!(milliseconds(frame("00:01:00;02").unwrap()).unwrap(), 60060);
        assert_eq!(milliseconds(frame("00:10:00;00").unwrap()).unwrap(), 599999);
        assert!(frame("00:01:00;00").is_err());
        assert!(frame("00:01:00;01").is_err());
        assert!(frame("00:00:00:30").is_err());
        assert!(frame("99:99:00:00").is_err());
        assert!(frame("00:00:0é:0").is_err());
    }
    #[test]
    fn malformed_parity_truncation_order_and_output_work_fail_closed() {
        assert!(parse("Scenarist_SCC V1.0\n00:00:00:00 1420\n", Some(1000)).is_err());
        assert!(parse("Scenarist_SCC V1.0\n00:00:00:00 942\n", Some(1000)).is_err());
        assert!(parse("Scenarist_SCC V1.0\n00:00:00:00\n", Some(1000)).is_err());
        let input = source(&[
            line("00:00:02:00", &[(0x14, 0x29), (b'A', 0)]),
            line("00:00:01:00", &[(0x14, 0x2c)]),
        ]);
        assert!(parse(&input, Some(4000)).is_err());
        let mut channel = Channel {
            mode: Mode::Paint,
            ..Channel::default()
        };
        channel.write('a');
        let mut budget = Budget {
            cues: MAX_CUES,
            bytes: 0,
        };
        channel.sync(0, None, &mut budget, false).unwrap();
        assert!(channel.sync(100, None, &mut budget, true).is_err());
        let mut budget = Budget {
            cues: 0,
            bytes: MAX_OUTPUT,
        };
        assert!(channel.sync(100, None, &mut budget, true).is_err());
    }
    #[test]
    fn loading_another_caption_preserves_display_until_swap_and_clips_media_end() {
        let input = source(&[
            line("00:00:00:00", &[(0x14, 0x20), (b'A', b'A'), (0x14, 0x2f)]),
            line("00:00:01:00", &[(0x14, 0x2e), (0x11, 0x40), (b'B', b'B')]),
            line("00:00:02:00", &[(0x14, 0x2f)]),
            line("00:00:04:00", &[(0x14, 0x2c)]),
        ]);
        let tracks = parse(&input, Some(3000)).unwrap();
        let cues = &tracks[0].1;
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "AA");
        assert_eq!(cues[0].end, 2002);
        assert_eq!(cues[1].text, "BB");
        assert_eq!((cues[1].start, cues[1].end), (2002, 3000));
    }
}
