//! Binary full cells and closed metadata; no escape sequences or pointer values.
use crate::render::View;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier},
};
use serde::{Deserialize, Serialize};
use std::io::Write;
use unicode_segmentation::UnicodeSegmentation as _;
use unicode_width::UnicodeWidthStr as _;

pub const CONTRACT: &str = "rsi.terminal.render";
pub const VERSION: u32 = 1;
pub const SERVICE: &str = "rsi.terminal.render";
pub const MAXIMUM_FRAGMENT: usize = 64 * 1024;
pub const MAXIMUM_FRAME: usize = 32 * 1024 * 1024;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub attachment: u64,
    pub presentation: u64,
    pub revision: u64,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub identity: Identity,
    pub width: u16,
    pub height: u16,
    pub bytes: usize,
}
impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        dimensions(self.width, self.height)?;
        if self.bytes == 0
            || self.bytes > crate::scene::MAXIMUM_SCENE_BYTES
            || self.identity.presentation == 0
            || self.identity.revision == 0
        {
            return Err("invalid render request");
        }
        Ok(())
    }
}
pub fn dimensions(width: u16, height: u16) -> Result<(), &'static str> {
    if width == 0 || width > 512 || height == 0 || height > 256 {
        Err("terminal dimension bound")
    } else {
        Ok(())
    }
}
pub fn json(value: &impl Serialize, maximum: usize) -> Result<Vec<u8>, &'static str> {
    struct Bounded {
        bytes: Vec<u8>,
        maximum: usize,
    }
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum - self.bytes.len() {
                return Err(std::io::Error::other("encoded byte bound"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut out = Bounded {
        bytes: Vec::new(),
        maximum,
    };
    serde_json::to_writer(&mut out, value).map_err(|_| "encoded byte bound")?;
    Ok(out.bytes)
}
fn color(color: Color) -> [u8; 4] {
    match color {
        Color::Reset => [0, 0, 0, 0],
        Color::Black => [1, 0, 0, 0],
        Color::Red => [2, 0, 0, 0],
        Color::Green => [3, 0, 0, 0],
        Color::Yellow => [4, 0, 0, 0],
        Color::Blue => [5, 0, 0, 0],
        Color::Magenta => [6, 0, 0, 0],
        Color::Cyan => [7, 0, 0, 0],
        Color::Gray => [8, 0, 0, 0],
        Color::DarkGray => [9, 0, 0, 0],
        Color::LightRed => [10, 0, 0, 0],
        Color::LightGreen => [11, 0, 0, 0],
        Color::LightYellow => [12, 0, 0, 0],
        Color::LightBlue => [13, 0, 0, 0],
        Color::LightMagenta => [14, 0, 0, 0],
        Color::LightCyan => [15, 0, 0, 0],
        Color::White => [16, 0, 0, 0],
        Color::Rgb(r, g, b) => [17, r, g, b],
        Color::Indexed(i) => [18, i, 0, 0],
    }
}
fn read_color(raw: &[u8]) -> Result<Color, &'static str> {
    if raw[0] < 17 && raw[1..] != [0, 0, 0] || raw[0] == 18 && raw[2..] != [0, 0] {
        return Err("invalid cell color");
    }
    Ok(match raw[0] {
        0 => Color::Reset,
        1 => Color::Black,
        2 => Color::Red,
        3 => Color::Green,
        4 => Color::Yellow,
        5 => Color::Blue,
        6 => Color::Magenta,
        7 => Color::Cyan,
        8 => Color::Gray,
        9 => Color::DarkGray,
        10 => Color::LightRed,
        11 => Color::LightGreen,
        12 => Color::LightYellow,
        13 => Color::LightBlue,
        14 => Color::LightMagenta,
        15 => Color::LightCyan,
        16 => Color::White,
        17 => Color::Rgb(raw[1], raw[2], raw[3]),
        18 => Color::Indexed(raw[1]),
        _ => return Err("invalid cell color"),
    })
}
pub fn encode(identity: Identity, buffer: &Buffer, view: &View) -> Result<Vec<u8>, &'static str> {
    dimensions(buffer.area.width, buffer.area.height)?;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RSIT\x01");
    for value in [
        identity.attachment,
        identity.presentation,
        identity.revision,
    ] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes.extend_from_slice(&buffer.area.width.to_le_bytes());
    bytes.extend_from_slice(&buffer.area.height.to_le_bytes());
    let map = json(view, 16 * 1024 * 1024)?;
    bytes.extend_from_slice(
        &u32::try_from(map.len())
            .map_err(|_| "source map bound")?
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&map);
    for cell in &buffer.content {
        let symbol = cell.symbol().as_bytes();
        let len = u16::try_from(symbol.len()).map_err(|_| "cell symbol bound")?;
        if bytes.len() + symbol.len() + 13 > MAXIMUM_FRAME {
            return Err("frame byte bound");
        }
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes.extend_from_slice(symbol);
        bytes.extend_from_slice(&color(cell.fg));
        bytes.extend_from_slice(&color(cell.bg));
        bytes.extend_from_slice(&cell.modifier.bits().to_le_bytes());
        bytes.push(match cell.diff_option {
            ratatui::buffer::CellDiffOption::None => 0,
            ratatui::buffer::CellDiffOption::Skip => 1,
            ratatui::buffer::CellDiffOption::AlwaysUpdate => 2,
            ratatui::buffer::CellDiffOption::ForcedWidth(_) => {
                return Err("unsupported forced cell width");
            }
        });
    }
    Ok(bytes)
}
struct Reader<'a> {
    bytes: &'a [u8],
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let slice = self.bytes.get(..n).ok_or("truncated frame")?;
        self.bytes = &self.bytes[n..];
        Ok(slice)
    }
    fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }
    fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }
    fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }
}
pub fn decode(bytes: &[u8], expected: &Request) -> Result<(Buffer, View), &'static str> {
    expected.validate()?;
    if bytes.len() > MAXIMUM_FRAME {
        return Err("frame byte bound");
    }
    let mut input = Reader { bytes };
    if input.take(5)? != b"RSIT\x01" {
        return Err("invalid frame version");
    }
    let identity = Identity {
        attachment: input.u64()?,
        presentation: input.u64()?,
        revision: input.u64()?,
    };
    let width = input.u16()?;
    let height = input.u16()?;
    if identity != expected.identity || (width, height) != (expected.width, expected.height) {
        return Err("stale terminal frame");
    }
    let map_len = input.u32()? as usize;
    if map_len > 16 * 1024 * 1024 {
        return Err("source map bound");
    }
    let view: View =
        serde_json::from_slice(input.take(map_len)?).map_err(|_| "invalid source map")?;
    view.validate(width, height)?;
    let mut buffer = Buffer::empty(Rect::new(0, 0, width, height));
    for cell in &mut buffer.content {
        let len = usize::from(input.u16()?);
        let symbol = std::str::from_utf8(input.take(len)?).map_err(|_| "invalid cell UTF-8")?;
        if symbol.is_empty()
            || symbol.graphemes(true).count() != 1
            || symbol.width() > 2
            || symbol.chars().any(char::is_control)
            || crate::terminal_text(symbol) != symbol
        {
            return Err("invalid cell symbol");
        }
        cell.set_symbol(symbol);
        cell.fg = read_color(input.take(4)?)?;
        cell.bg = read_color(input.take(4)?)?;
        cell.modifier = Modifier::from_bits(input.u16()?).ok_or("invalid cell modifier")?;
        cell.set_diff_option(match input.take(1)?[0] {
            0 => ratatui::buffer::CellDiffOption::None,
            1 => ratatui::buffer::CellDiffOption::Skip,
            2 => ratatui::buffer::CellDiffOption::AlwaysUpdate,
            _ => return Err("invalid cell flag"),
        });
    }
    if !input.bytes.is_empty() {
        return Err("trailing frame bytes");
    }
    Ok((buffer, view))
}

/// The model carries only source metadata; display text travels in bounded raw chunks.
pub fn request_header(request: &Request) -> Result<Vec<u8>, &'static str> {
    let model = rsi_ui_protocol::UiModel {
        renderer: "rsi.terminal.cells".into(),
        schema: rsi_ui_protocol::ModelSchema {
            name: "rsi.terminal.scene".into(),
            version: 1,
        },
        data: serde_json::to_value(request).map_err(|_| "render metadata")?,
        actions: vec![],
        sources: vec![rsi_ui_protocol::ModelSource {
            name: "scene".into(),
            title: "Terminal viewport".into(),
            media_type: "application/vnd.rsi.terminal.scene+json".into(),
        }],
        standard_view: None,
    };
    model.validate().map_err(|_| "render model")?;
    json(&model, rsi_ui_protocol::MAXIMUM_VIEW_BYTES)
}
pub fn parse_header(bytes: &[u8]) -> Result<Request, &'static str> {
    if bytes.len() > rsi_ui_protocol::MAXIMUM_VIEW_BYTES {
        return Err("render model bound");
    }
    let model: rsi_ui_protocol::UiModel =
        serde_json::from_slice(bytes).map_err(|_| "render model")?;
    model.validate().map_err(|_| "render model")?;
    if model.renderer != "rsi.terminal.cells"
        || model.schema.name != "rsi.terminal.scene"
        || model.schema.version != 1
        || !model.actions.is_empty()
        || model.sources.len() != 1
        || model.sources[0].name != "scene"
        || model.standard_view.is_some()
    {
        return Err("unsupported render model");
    }
    let request: Request = serde_json::from_value(model.data).map_err(|_| "render metadata")?;
    request.validate()?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_cells_reject_stale_dimensions_truncation_styles_and_terminal_controls() {
        let request = Request {
            identity: Identity {
                attachment: 7,
                presentation: 2,
                revision: 3,
            },
            width: 2,
            height: 1,
            bytes: 1,
        };
        let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
        buffer[(0, 0)].set_symbol("中");
        buffer[(1, 0)].fg = Color::Rgb(1, 2, 3);
        let encoded = encode(request.identity, &buffer, &View::default()).unwrap();
        assert_eq!(decode(&encoded, &request).unwrap().0, buffer);
        for end in [0, 4, 8, 24, 32, encoded.len() - 1] {
            assert!(decode(&encoded[..end], &request).is_err());
        }
        let mut stale = Request {
            identity: request.identity,
            width: 2,
            height: 1,
            bytes: 1,
        };
        stale.identity.presentation += 1;
        assert!(decode(&encoded, &stale).is_err());
        stale.identity = request.identity;
        stale.width = 3;
        assert!(decode(&encoded, &stale).is_err());
        for text in ["\x1b[2J", "\n", "two", "\u{202e}"] {
            buffer[(0, 0)].set_symbol(text);
            assert!(
                decode(
                    &encode(request.identity, &buffer, &View::default()).unwrap(),
                    &request
                )
                .is_err()
            );
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(decode(&trailing, &request).is_err());
        assert!(parse_header(br#"{"renderer":"unadmitted"}"#).is_err());
        let header = request_header(&request).unwrap();
        assert!(header.len() < 128 * 1024);
        assert_eq!(parse_header(&header).unwrap().identity, request.identity);
    }
}
